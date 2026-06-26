//! Interactive terminal UI for exploring the scan results live.
//!
//! Architecture: the Redis scan runs in a background tokio task that publishes
//! a fresh `Stats` snapshot into shared state after every batch. The UI runs on
//! a dedicated blocking thread, polling that shared state ~15×/s and redrawing.
//! Keyboard events are read with a short timeout so the UI stays responsive
//! while the scan fills in.

use crate::stats::Stats;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use humansize::{format_size, DECIMAL};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Alignment;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Gauge, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs,
};
use ratatui::{Frame, Terminal};
use std::io::Stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Shared, mutable snapshot the scan task writes and the UI reads.
#[derive(Default)]
pub struct Shared {
    pub stats: Stats,
    pub done: bool,
    /// Set if the scan task failed; shown in the footer.
    pub error: Option<String>,
    /// Whether this scan collected the advanced stats (memory/type/ttl/biggest).
    /// Drives labels and the biggest-keys empty-state hint.
    pub advanced: bool,
}

pub type SharedHandle = Arc<Mutex<Shared>>;

/// Which sort order to apply to namespace/type tables.
#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Count,
    TotalMem,
    AvgMem,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Count => "count",
            Sort::TotalMem => "total mem",
            Sort::AvgMem => "avg mem",
        }
    }
    fn next(self) -> Sort {
        match self {
            Sort::Count => Sort::TotalMem,
            Sort::TotalMem => Sort::AvgMem,
            Sort::AvgMem => Sort::Count,
        }
    }
}

/// What the whole screen is showing.
enum Screen {
    /// In-TUI scan-depth selection. `selected` is the highlighted option index
    /// (0 = basic, 1 = advanced).
    Picking { selected: usize },
    /// The normal scan + browse experience.
    Running,
    /// Drill-down: the sample key list for a chosen type.
    Keys,
    /// Drill-down: a single key's value.
    Value,
}

/// Top-level focus: which panel/view is active. These are the tabs in the bar.
#[derive(Clone, Copy, PartialEq)]
enum View {
    /// Namespace list (left) + its type breakdown (right).
    Browse,
    /// Full-screen biggest-keys table.
    Biggest,
    /// Persistent (no-TTL) keys: how much memory expiry can't free.
    Persistent,
}

impl View {
    fn title(self) -> &'static str {
        match self {
            View::Browse => "Browse",
            View::Biggest => "Biggest keys",
            View::Persistent => "Persistent",
        }
    }

    /// The tabs available in bar order. The advanced-only views (Biggest,
    /// Persistent) need memory/TTL data, so they're hidden in basic mode.
    fn available(advanced: bool) -> Vec<View> {
        if advanced {
            vec![View::Browse, View::Biggest, View::Persistent]
        } else {
            vec![View::Browse]
        }
    }

    /// Index of this view within the available tabs (0 if somehow absent).
    fn index(self, advanced: bool) -> usize {
        View::available(advanced)
            .iter()
            .position(|&v| v == self)
            .unwrap_or(0)
    }

    /// Next/previous tab among the available ones, wrapping.
    fn cycle(self, advanced: bool, forward: bool) -> View {
        let tabs = View::available(advanced);
        let i = self.index(advanced) as isize;
        let delta = if forward { 1 } else { -1 };
        let next = (i + delta).rem_euclid(tabs.len() as isize) as usize;
        tabs[next]
    }
}

/// Within the Browse view, which of the two panes has keyboard focus.
#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Namespaces,
    Types,
}

pub struct App {
    shared: SharedHandle,
    /// Whether we're on the depth picker or the running UI.
    screen: Screen,
    /// Fires the chosen depth (advanced?) to the waiting scan task. Consumed on
    /// the first selection; `None` once a scan is already underway.
    mode_tx: Option<oneshot::Sender<bool>>,
    view: View,
    /// Which Browse pane the arrow keys drive (Namespaces or Types).
    pane: Pane,
    sort: Sort,
    /// Selection index into the (filtered, sorted) namespace list.
    ns_state: ListState,
    /// Selection into the type table of the focused namespace.
    type_state: TableState,
    /// Selection into the biggest-keys table.
    big_state: TableState,
    /// Selection into the persistent-keys table.
    persist_state: TableState,
    /// Live substring filter (applies to namespace + type names).
    filter: String,
    /// True while the filter input box is capturing keystrokes.
    editing_filter: bool,
    /// Whether the scan collected advanced stats (set once chosen).
    advanced: bool,

    // --- drill-down (key list + value inspector) ---
    /// Channel to the async fetcher; `None` if no live connection is available.
    fetch_tx: Option<mpsc::Sender<crate::inspect::Request>>,
    /// Shared slot the fetcher writes results into.
    inbox: Option<crate::inspect::InboxHandle>,
    /// Last inbox generation we consumed, to detect fresh results.
    last_gen: u64,
    /// Currently displayed key list (after drilling into a type).
    key_list: Option<crate::inspect::KeyList>,
    key_list_state: ListState,
    /// Currently displayed value (after drilling into a key).
    key_value: Option<crate::inspect::KeyValue>,
    /// Vertical scroll offset within the value body.
    value_scroll: u16,
    /// Show the hex dump (true) vs the text rendering (false) for a string.
    value_hex: bool,
    /// True while a fetch is in flight.
    loading: bool,

    /// Live server stats (memory/evictions/…) polled in the background.
    info: Option<crate::serverinfo::InfoHandle>,

    should_quit: bool,
}

impl App {
    /// App for an already-decided scan (flags/`--mode` given): jumps straight to
    /// the running UI; `advanced` reflects the depth.
    pub fn new(shared: SharedHandle) -> Self {
        let advanced = shared.lock().unwrap().advanced;
        Self::build(shared, Screen::Running, None, advanced)
    }

    /// App that opens on the in-TUI depth picker. The chosen depth is sent on
    /// `mode_tx` to start the scan.
    pub fn with_picker(shared: SharedHandle, mode_tx: oneshot::Sender<bool>) -> Self {
        Self::build(
            shared,
            Screen::Picking { selected: 0 },
            Some(mode_tx),
            false,
        )
    }

    fn build(
        shared: SharedHandle,
        screen: Screen,
        mode_tx: Option<oneshot::Sender<bool>>,
        advanced: bool,
    ) -> Self {
        let mut ns_state = ListState::default();
        ns_state.select(Some(0));
        App {
            shared,
            screen,
            mode_tx,
            view: View::Browse,
            pane: Pane::Namespaces,
            sort: Sort::Count,
            ns_state,
            type_state: TableState::default(),
            big_state: TableState::default(),
            persist_state: TableState::default(),
            filter: String::new(),
            editing_filter: false,
            advanced,
            fetch_tx: None,
            inbox: None,
            last_gen: 0,
            key_list: None,
            key_list_state: ListState::default(),
            key_value: None,
            value_scroll: 0,
            value_hex: false,
            loading: false,
            info: None,
            should_quit: false,
        }
    }

    /// Give the app a live drill-down fetcher. Without this, Enter on a type is
    /// a no-op (e.g. in tests that don't run a fetcher).
    pub fn attach_inspector(
        &mut self,
        fetch_tx: mpsc::Sender<crate::inspect::Request>,
        inbox: crate::inspect::InboxHandle,
    ) {
        self.fetch_tx = Some(fetch_tx);
        self.inbox = Some(inbox);
    }

    /// Give the app the handle the background poller writes server stats into.
    pub fn attach_server_info(&mut self, info: crate::serverinfo::InfoHandle) {
        self.info = Some(info);
    }

    /// Consume any fresh result the fetcher published and move to the matching
    /// screen. Cheap no-op when nothing new arrived.
    fn poll_inbox(&mut self) {
        let Some(inbox) = &self.inbox else { return };
        let (gen, loading, key_list, key_value) = {
            let g = inbox.lock().unwrap();
            (
                g.generation,
                g.loading,
                g.key_list.clone(),
                g.key_value.clone(),
            )
        };
        self.loading = loading;
        if gen == self.last_gen {
            return;
        }
        self.last_gen = gen;

        // A new value result takes us to the Value screen; a new key-list result
        // to the Keys screen. We only switch when we're waiting on that kind of
        // result, so a late reply can't yank the user out of where they navigated.
        if let Some(kv) = key_value {
            if matches!(self.screen, Screen::Keys | Screen::Value) {
                // Default to hex for binary values; text otherwise.
                self.value_hex = kv.is_binary && kv.hex.is_some();
                self.key_value = Some(kv);
                self.value_scroll = 0;
                self.screen = Screen::Value;
            }
        }
        if let Some(kl) = key_list {
            if matches!(self.screen, Screen::Running | Screen::Keys) {
                let had = kl.keys.len();
                self.key_list = Some(kl);
                self.key_list_state
                    .select(if had == 0 { None } else { Some(0) });
                self.screen = Screen::Keys;
            }
        }
    }

    /// Request the sample key list for the currently selected type.
    fn drill_into_type(&mut self, snapshot: &Stats) {
        let Some(tx) = &self.fetch_tx else { return };
        let ns_rows = filtered_namespaces(snapshot, &self.filter, self.sort);
        let Some(ns) = self.ns_state.selected().and_then(|i| ns_rows.get(i)) else {
            return;
        };
        let types = filtered_types(snapshot, &ns.name, &self.filter, self.sort);
        let Some(t) = self.type_state.selected().and_then(|i| types.get(i)) else {
            return;
        };
        self.loading = true;
        let _ = tx.try_send(crate::inspect::Request::ListKeys {
            namespace: ns.name.clone(),
            key_type: t.name.clone(),
        });
    }

    /// Request the value of the currently selected key.
    fn drill_into_key(&mut self) {
        let Some(tx) = &self.fetch_tx else { return };
        let Some(list) = &self.key_list else { return };
        let Some(key) = self
            .key_list_state
            .selected()
            .and_then(|i| list.keys.get(i))
        else {
            return;
        };
        self.loading = true;
        let _ = tx.try_send(crate::inspect::Request::GetValue { key: key.clone() });
    }
}

/// Run the TUI event loop until the user quits. Restores the terminal on exit
/// (including on error / panic-free early return).
pub fn run(app: &mut App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(app, &mut terminal);

    // Always restore the terminal, even if the loop errored.
    disable_raw_mode().ok();
    terminal.backend_mut().execute(LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    loop {
        // Pull any fresh drill-down results the fetcher produced.
        app.poll_inbox();

        // Take a cheap snapshot under the lock, then render lock-free.
        let (snapshot, done, error) = {
            let g = app.shared.lock().unwrap();
            (g.stats.clone(), g.done, g.error.clone())
        };

        terminal.draw(|f| draw(f, app, &snapshot, done, error.as_deref()))?;

        // Poll for input but wake up regularly to refresh live counts.
        if event::poll(Duration::from_millis(66))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(app, key.code, key.modifiers, &snapshot);
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers, snapshot: &Stats) {
    let ctrl_c = matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL);

    // The depth picker captures input until a choice is made.
    if let Screen::Picking { selected } = &mut app.screen {
        match code {
            KeyCode::Char('q') => app.should_quit = true,
            _ if ctrl_c => app.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => *selected = 0,
            KeyCode::Down | KeyCode::Char('j') => *selected = 1,
            KeyCode::Char('b') => choose_mode(app, false),
            KeyCode::Char('a') => choose_mode(app, true),
            KeyCode::Enter => {
                let advanced = *selected == 1;
                choose_mode(app, advanced);
            }
            _ => {}
        }
        return;
    }

    // Drill-down: key list of a type.
    if matches!(app.screen, Screen::Keys) {
        match code {
            KeyCode::Char('q') => app.should_quit = true,
            _ if ctrl_c => app.should_quit = true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                app.screen = Screen::Running; // back to the type list
            }
            KeyCode::Down | KeyCode::Char('j') => move_key_list(app, 1),
            KeyCode::Up | KeyCode::Char('k') => move_key_list(app, -1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => app.drill_into_key(),
            _ => {}
        }
        return;
    }

    // Drill-down: a single key's value.
    if matches!(app.screen, Screen::Value) {
        match code {
            KeyCode::Char('q') => app.should_quit = true,
            _ if ctrl_c => app.should_quit = true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                app.screen = Screen::Keys; // back to the key list
                app.key_value = None;
            }
            // Toggle hex <-> text (only when a hex rendering exists, i.e. strings).
            KeyCode::Char('x') => {
                if app
                    .key_value
                    .as_ref()
                    .and_then(|v| v.hex.as_ref())
                    .is_some()
                {
                    app.value_hex = !app.value_hex;
                    app.value_scroll = 0;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.value_scroll = app.value_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.value_scroll = app.value_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => app.value_scroll = app.value_scroll.saturating_add(20),
            KeyCode::PageUp => app.value_scroll = app.value_scroll.saturating_sub(20),
            _ => {}
        }
        return;
    }

    // While editing the filter, most keys feed the filter string.
    if app.editing_filter {
        match code {
            KeyCode::Esc | KeyCode::Enter => app.editing_filter = false,
            KeyCode::Backspace => {
                app.filter.pop();
            }
            KeyCode::Char(c) => app.filter.push(c),
            _ => {}
        }
        // Reset selection so it stays in range as the filtered list shrinks,
        // and return focus to the namespace list (the type set just changed).
        app.ns_state.select(Some(0));
        app.type_state.select(Some(0));
        app.pane = Pane::Namespaces;
        return;
    }

    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        KeyCode::Char('/') => {
            app.editing_filter = true;
        }
        KeyCode::Char('s') => app.sort = app.sort.next(),
        // Tab / Shift-Tab switch the top-level tabs. `b` is a forward alias.
        KeyCode::Tab => app.view = app.view.cycle(app.advanced, true),
        KeyCode::BackTab => app.view = app.view.cycle(app.advanced, false),
        KeyCode::Char('b') => app.view = app.view.cycle(app.advanced, true),
        // Within Browse, move focus between the Namespaces and Types panes.
        KeyCode::Right | KeyCode::Char('l') => focus_pane(app, snapshot, Pane::Types),
        KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
            focus_pane(app, snapshot, Pane::Namespaces)
        }
        // Enter drills in: from Namespaces into the Types pane, and from a
        // focused type into its sample key list.
        KeyCode::Enter => match app.view {
            View::Browse if app.pane == Pane::Namespaces => focus_pane(app, snapshot, Pane::Types),
            View::Browse => app.drill_into_type(snapshot),
            // Biggest/Persistent are read-only tables — nothing to drill into.
            View::Biggest | View::Persistent => {}
        },
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, snapshot, 1),
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, snapshot, -1),
        _ => {}
    }
}

/// Move the selection within the drill-down key list (wraps).
fn move_key_list(app: &mut App, delta: isize) {
    let Some(list) = &app.key_list else { return };
    let len = list.keys.len();
    if len == 0 {
        return;
    }
    let cur = app.key_list_state.selected().unwrap_or(0) as isize;
    let next = (cur + delta).rem_euclid(len as isize) as usize;
    app.key_list_state.select(Some(next));
}

/// Commit the depth choice: tell the waiting scan task to start, record the
/// mode, and switch to the running UI.
fn choose_mode(app: &mut App, advanced: bool) {
    app.advanced = advanced;
    if let Ok(mut g) = app.shared.lock() {
        g.advanced = advanced;
    }
    if let Some(tx) = app.mode_tx.take() {
        // Receiver may be gone only if the scan task died; ignore the error.
        let _ = tx.send(advanced);
    }
    app.screen = Screen::Running;
}

/// Move Browse focus to `target`, but only into Types when there actually are
/// types to look at (so focus never lands on an empty pane).
fn focus_pane(app: &mut App, snapshot: &Stats, target: Pane) {
    if app.view != View::Browse {
        return;
    }
    if target == Pane::Types {
        if type_rows_for_selection(app, snapshot).is_empty() {
            return;
        }
        // Entering the Types pane: ensure a row is selected.
        if app.type_state.selected().is_none() {
            app.type_state.select(Some(0));
        }
    }
    app.pane = target;
}

/// The type rows for the currently selected namespace (respects filter+sort).
fn type_rows_for_selection(app: &App, snapshot: &Stats) -> Vec<TypeRow> {
    let ns_rows = filtered_namespaces(snapshot, &app.filter, app.sort);
    match app.ns_state.selected().and_then(|i| ns_rows.get(i)) {
        Some(ns) => filtered_types(snapshot, &ns.name, &app.filter, app.sort),
        None => Vec::new(),
    }
}

fn move_selection(app: &mut App, snapshot: &Stats, delta: isize) {
    let step = |cur: usize, len: usize| ((cur as isize + delta).rem_euclid(len as isize)) as usize;
    match app.view {
        View::Browse => match app.pane {
            Pane::Namespaces => {
                let len = filtered_namespaces(snapshot, &app.filter, app.sort).len();
                if len == 0 {
                    return;
                }
                let next = step(app.ns_state.selected().unwrap_or(0), len);
                app.ns_state.select(Some(next));
                // Moving to a different namespace resets the type cursor.
                app.type_state.select(Some(0));
            }
            Pane::Types => {
                let len = type_rows_for_selection(app, snapshot).len();
                if len == 0 {
                    return;
                }
                let next = step(app.type_state.selected().unwrap_or(0), len);
                app.type_state.select(Some(next));
            }
        },
        View::Biggest => {
            let len = snapshot.biggest().len();
            if len == 0 {
                return;
            }
            let next = step(app.big_state.selected().unwrap_or(0), len);
            app.big_state.select(Some(next));
        }
        View::Persistent => {
            let len = persistent_rows(snapshot).len();
            if len == 0 {
                return;
            }
            let next = step(app.persist_state.selected().unwrap_or(0), len);
            app.persist_state.select(Some(next));
        }
    }
}

/// A persistent (no-TTL) row: which type holds unevictable keys, and how much.
struct PersistRow {
    namespace: String,
    key_type: String,
    keys: u64,
    bytes: u64,
}

/// Every (namespace, type) bucket that has persistent keys, sorted by the memory
/// it holds (descending) — the space expiry can't free.
fn persistent_rows(stats: &Stats) -> Vec<PersistRow> {
    let mut rows: Vec<PersistRow> = stats
        .namespaces
        .iter()
        .flat_map(|(ns, types)| {
            types
                .iter()
                .filter(|(_, s)| s.persistent > 0)
                .map(move |(t, s)| PersistRow {
                    namespace: ns.clone(),
                    key_type: t.clone(),
                    keys: s.persistent,
                    bytes: s.persistent_bytes,
                })
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse((r.bytes, r.keys)));
    rows
}

// ---------------------------------------------------------------------------
// Data shaping helpers
// ---------------------------------------------------------------------------

/// A namespace row ready for display.
struct NsRow {
    name: String,
    keys: u64,
    bytes: u64,
}

fn filtered_namespaces(stats: &Stats, filter: &str, sort: Sort) -> Vec<NsRow> {
    let f = filter.to_lowercase();
    let mut rows: Vec<NsRow> = stats
        .namespaces
        .keys()
        .filter(|ns| f.is_empty() || ns.to_lowercase().contains(&f))
        .map(|ns| NsRow {
            name: ns.clone(),
            keys: stats.namespace_count(ns),
            bytes: stats.namespace_bytes(ns),
        })
        .collect();
    sort_rows(&mut rows, sort, |r| (r.keys, r.bytes, r.bytes));
    rows
}

/// A type row within a namespace.
struct TypeRow {
    name: String,
    keys: u64,
    total_bytes: u64,
    avg_bytes: u64,
    data_types: String,
    persistent: u64,
    expiring: u64,
}

fn filtered_types(stats: &Stats, ns: &str, filter: &str, sort: Sort) -> Vec<TypeRow> {
    let f = filter.to_lowercase();
    let Some(types) = stats.namespaces.get(ns) else {
        return Vec::new();
    };
    let mut rows: Vec<TypeRow> = types
        .iter()
        .filter(|(name, _)| {
            // Match the canonical name OR its humanized form, so typing "tag"
            // finds the \x01tags\x01… sets shown as "🏷 … (tag set)".
            f.is_empty()
                || name.to_lowercase().contains(&f)
                || display_type(name).to_lowercase().contains(&f)
        })
        .map(|(name, s)| {
            let mut dts: Vec<_> = s.data_types.iter().collect();
            dts.sort_by(|a, b| b.1.cmp(a.1));
            let data_types = dts
                .iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect::<Vec<_>>()
                .join(" ");
            TypeRow {
                name: name.clone(),
                keys: s.count,
                total_bytes: s.total_bytes,
                avg_bytes: s.avg_bytes(),
                data_types,
                persistent: s.persistent,
                expiring: s.expiring,
            }
        })
        .collect();
    sort_rows(&mut rows, sort, |r| (r.keys, r.total_bytes, r.avg_bytes));
    rows
}

/// Sort `rows` descending by the metric the current `sort` selects. `key`
/// returns (count, total_bytes, avg_bytes) for a row.
fn sort_rows<T>(rows: &mut [T], sort: Sort, key: impl Fn(&T) -> (u64, u64, u64)) {
    rows.sort_by(|a, b| {
        let (ac, at, aa) = key(a);
        let (bc, bt, ba) = key(b);
        match sort {
            Sort::Count => bc.cmp(&ac),
            Sort::TotalMem => bt.cmp(&at),
            Sort::AvgMem => ba.cmp(&aa),
        }
    });
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "0.0%".into()
    } else {
        format!("{:.1}%", part as f64 / whole as f64 * 100.0)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, app: &mut App, stats: &Stats, done: bool, error: Option<&str>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header: scan gauge (3) + server stats (1)
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer / help
        ])
        .split(f.area());

    // The depth picker shares the same frame (header + footer) so the transition
    // into the running UI doesn't look like a different program.
    if let Screen::Picking { selected } = app.screen {
        draw_picker_header(f, chunks[0]);
        draw_picker(f, chunks[1], selected);
        draw_picker_footer(f, chunks[2]);
        return;
    }

    // Drill-down screens reuse the header band; they have their own body+footer.
    match app.screen {
        Screen::Keys => {
            draw_drill_header(f, chunks[0], "Keys");
            draw_key_list(f, chunks[1], app);
            draw_drill_footer(
                f,
                chunks[2],
                "↑↓ move  Enter view value  ←/Esc back  q quit",
            );
            return;
        }
        Screen::Value => {
            draw_drill_header(f, chunks[0], "Value");
            draw_value(f, chunks[1], app);
            draw_drill_footer(
                f,
                chunks[2],
                "↑↓/PgUp/PgDn scroll  x: hex/text  ←/Esc back  q quit",
            );
            return;
        }
        _ => {}
    }

    // Split the header band into the scan gauge (3 rows) and a server-stats
    // line (1 row).
    let head = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(1)])
        .split(chunks[0]);
    draw_header(f, head[0], stats, done, error.is_some(), app.advanced);
    draw_server_stats(f, head[1], app);

    // Body = a one-row tab bar + the active view below it.
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(chunks[1]);
    draw_tab_bar(f, body[0], app.view, app.advanced);
    match app.view {
        View::Browse => draw_browse(f, body[1], app, stats),
        View::Biggest => draw_biggest(f, body[1], app, stats),
        View::Persistent => draw_persistent(f, body[1], app, stats),
    }
    draw_footer(f, chunks[2], app, error);
}

/// The top-level tab strip.
fn draw_tab_bar(f: &mut Frame, area: Rect, view: View, advanced: bool) {
    let titles: Vec<&str> = View::available(advanced)
        .iter()
        .map(|v| v.title())
        .collect();
    let tabs = Tabs::new(titles)
        .select(view.index(advanced))
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│")
        .padding(" ", " ");
    f.render_widget(tabs, area);
}

fn draw_drill_header(f: &mut Frame, area: Rect, what: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" shopware-redis-cli-helper ");
    let p = Paragraph::new(Line::from(Span::styled(
        format!("Inspecting · {what}"),
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .block(block);
    f.render_widget(p, area);
}

fn draw_drill_footer(f: &mut Frame, area: Rect, hint: &str) {
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {hint} "),
        Style::default().fg(Color::Gray),
    )));
    f.render_widget(p, area);
}

/// The sample key list drilled from a type.
fn draw_key_list(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(list) = &app.key_list else {
        let msg = if app.loading {
            "Loading keys…"
        } else {
            "No keys."
        };
        f.render_widget(
            Paragraph::new(msg).block(Block::default().borders(Borders::ALL).title(" Keys ")),
            area,
        );
        return;
    };

    if let Some(err) = &list.error {
        f.render_widget(
            Paragraph::new(format!("Error: {err}"))
                .style(Style::default().fg(Color::Red))
                .block(Block::default().borders(Borders::ALL).title(" Keys ")),
            area,
        );
        return;
    }

    let title = format!(
        " {} · {} — {} keys{} ",
        list.namespace,
        display_type(&list.key_type),
        list.keys.len(),
        if list.capped { " (sample capped)" } else { "" }
    );
    let items: Vec<ListItem> = list
        .keys
        .iter()
        .map(|k| {
            ListItem::new(Line::from(truncate(
                k,
                area.width.saturating_sub(4) as usize,
            )))
        })
        .collect();
    let widget = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(widget, area, &mut app.key_list_state);
}

/// The value of a single key (type-aware, decompressed where applicable).
fn draw_value(f: &mut Frame, area: Rect, app: &App) {
    let Some(v) = &app.key_value else {
        let msg = if app.loading {
            "Loading value…"
        } else {
            "No value."
        };
        f.render_widget(
            Paragraph::new(msg).block(Block::default().borders(Borders::ALL).title(" Value ")),
            area,
        );
        return;
    };

    // Split into a metadata header band and the scrollable body. The header
    // holds a border (2 rows) plus key + two metadata lines.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(0)])
        .split(area);

    let mut meta: Vec<Span> = vec![
        Span::styled("type ", Style::default().fg(Color::DarkGray)),
        Span::styled(&v.redis_type, Style::default().fg(Color::Cyan)),
    ];
    if let Some(enc) = v.encoding {
        if enc != crate::value::Encoding::Plain {
            // Note an embedded stream (offset > 0) — e.g. a Symfony cache
            // wrapper around the compressed inner value.
            let label = match v.stream_offset {
                Some(off) if off > 0 => {
                    format!("decompressed {} (embedded at offset {off})", enc.label())
                }
                _ => format!("decompressed from {}", enc.label()),
            };
            meta.push(Span::raw("   "));
            meta.push(Span::styled(label, Style::default().fg(Color::Yellow)));
        }
    }
    let mut meta2: Vec<Span> = Vec::new();
    if let Some(b) = v.size_bytes {
        meta2.push(Span::styled(
            format!("size {}   ", format_size(b, DECIMAL)),
            Style::default().fg(Color::Magenta),
        ));
    }
    if let Some(n) = v.elements {
        meta2.push(Span::styled(format!("elements {n}   "), Style::default()));
    }
    meta2.push(Span::styled(
        format!("ttl {}", crate::inspect::ttl_with_expiry(v.ttl)),
        Style::default(),
    ));

    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            truncate(&v.key, area.width.saturating_sub(4) as usize),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(meta),
        Line::from(meta2),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Key "));
    f.render_widget(header, rows[0]);

    // Pick the rendering: hex when toggled on and available, else text.
    let show_hex = app.value_hex && v.hex.is_some();
    let (body, title) = if let Some(err) = &v.error {
        (
            Paragraph::new(format!("Error: {err}")).style(Style::default().fg(Color::Red)),
            " Value ".to_string(),
        )
    } else if show_hex {
        let hex = v.hex.as_deref().unwrap_or("");
        (
            Paragraph::new(hex),
            format!(
                " Value · hex · {} bytes  (x: show text) ",
                v.size_bytes.map(|b| b.to_string()).unwrap_or_default()
            ),
        )
    } else {
        let toggle = if v.hex.is_some() {
            "  (x: show hex)"
        } else {
            ""
        };
        let tag = if v.is_binary {
            " · binary, shown as text"
        } else {
            ""
        };
        (
            Paragraph::new(v.body.as_str()),
            format!(" Value{tag}{toggle} "),
        )
    };
    f.render_widget(
        body.block(Block::default().borders(Borders::ALL).title(title))
            .scroll((app.value_scroll, 0)),
        rows[1],
    );
}

/// Header shown on the picker screen (same band as the scan gauge).
fn draw_picker_header(f: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" shopware-redis-cli-helper ");
    let p = Paragraph::new(Line::from(Span::styled(
        "Choose scan depth",
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .block(block)
    .alignment(Alignment::Center);
    f.render_widget(p, area);
}

fn draw_picker_footer(f: &mut Frame, area: Rect) {
    let p = Paragraph::new(Line::from(Span::styled(
        " ↑↓ choose  Enter confirm  (b basic / a advanced)  q: quit ",
        Style::default().fg(Color::Gray),
    )));
    f.render_widget(p, area);
}

/// The two-option depth selector, centered in the body area.
fn draw_picker(f: &mut Frame, area: Rect, selected: usize) {
    let options = [
        (
            "Basic",
            "Key counts only — one SCAN pass, no per-key round-trips.",
            "Fast, even on a database with millions of keys.",
        ),
        (
            "Advanced",
            "Adds memory, data type, TTL, and the biggest-keys leaderboard.",
            "Richer, but issues extra commands per key — slower on a huge DB.",
        ),
    ];

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, (title, line1, line2))| {
            let marker = if i == selected { "▶ " } else { "  " };
            let title_style = if i == selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(marker, Style::default().fg(Color::Cyan)),
                    Span::styled(*title, title_style),
                ]),
                Line::from(Span::styled(format!("    {line1}"), Style::default())),
                Line::from(Span::styled(
                    format!("    {line2}"),
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Line::from(""),
            ])
        })
        .collect();

    // Center a fixed-size box within the body.
    let inner = centered_rect(72, 12, area);
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(" Scan depth "));
    f.render_widget(list, inner);
}

/// A rect of `width`×`height` centered inside `area` (clamped to fit).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn draw_header(f: &mut Frame, area: Rect, stats: &Stats, done: bool, failed: bool, advanced: bool) {
    // Three states: failed, connecting (done==false and nothing seen yet),
    // and scanning/complete.
    let (label, color, ratio) = if failed {
        (
            "Connection failed — see message below".to_string(),
            Color::Red,
            1.0,
        )
    } else if !done && stats.total_keys == 0 {
        ("Connecting…".to_string(), Color::Yellow, 0.0)
    } else if done {
        (
            format!("Scan complete — {} keys", stats.total_keys),
            Color::Green,
            1.0,
        )
    } else {
        (
            format!("Scanning… {} keys so far", stats.total_keys),
            Color::Cyan,
            0.0,
        )
    };
    let title = format!(
        " shopware-redis-cli-helper · {} ",
        if advanced { "advanced" } else { "basic" }
    );
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(title))
        .gauge_style(Style::default().fg(color))
        .ratio(ratio)
        .label(label);
    f.render_widget(gauge, area);
}

/// The one-line server-stats strip below the scan gauge: how full Redis is,
/// plus eviction pressure, DB size, fragmentation, and version. Polled live.
fn draw_server_stats(f: &mut Frame, area: Rect, app: &App) {
    let Some(info) = app.info.as_ref().and_then(|h| h.lock().unwrap().clone()) else {
        let p = Paragraph::new(Line::from(Span::styled(
            " redis: connecting…",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(p, area);
        return;
    };

    let mut spans: Vec<Span> = vec![Span::styled(
        " redis ",
        Style::default().fg(Color::DarkGray),
    )];

    // Memory: a fullness % when a ceiling is set, else absolute usage.
    match (info.memory_fullness(), info.used_memory, info.maxmemory) {
        (Some(frac), Some(used), Some(max)) => {
            // Colour by pressure: green < 75%, yellow < 90%, red beyond.
            let color = if frac >= 0.90 {
                Color::Red
            } else if frac >= 0.75 {
                Color::Yellow
            } else {
                Color::Green
            };
            spans.push(Span::styled(
                format!("mem {:.0}% ", frac * 100.0),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(
                    "({}/{})  ",
                    format_size(used, DECIMAL),
                    format_size(max, DECIMAL)
                ),
                Style::default().fg(Color::Gray),
            ));
        }
        (None, Some(used), _) => {
            spans.push(Span::styled(
                format!("mem {}  ", format_size(used, DECIMAL)),
                Style::default().fg(Color::Magenta),
            ));
            spans.push(Span::styled(
                "(no limit)  ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        _ => {}
    }

    if let Some(keys) = info.db_keys {
        spans.push(Span::styled(
            format!("keys {keys}  "),
            Style::default().fg(Color::Cyan),
        ));
    }
    if let Some(ev) = info.evicted_keys {
        // Any eviction at all is worth flagging (cache thrashing the ceiling).
        let color = if ev > 0 { Color::Yellow } else { Color::Gray };
        spans.push(Span::styled(
            format!("evicted {ev}  "),
            Style::default().fg(color),
        ));
    }
    if let Some(frag) = info.mem_fragmentation_ratio {
        // >1.5 means RSS is much larger than logical use — wasted memory.
        let color = if frag >= 1.5 {
            Color::Yellow
        } else {
            Color::Gray
        };
        spans.push(Span::styled(
            format!("frag {frag:.2}  "),
            Style::default().fg(color),
        ));
    }
    if let Some(v) = &info.redis_version {
        spans.push(Span::styled(
            format!("v{v}"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Border style for a pane: bright cyan when focused, dim gray otherwise.
fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_browse(f: &mut Frame, area: Rect, app: &mut App, stats: &Stats) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);

    let ns_focused = app.pane == Pane::Namespaces;
    let types_focused = app.pane == Pane::Types;

    let ns_rows = filtered_namespaces(stats, &app.filter, app.sort);

    // Keep a valid selection: clamp into range, and auto-select the first row
    // once namespaces appear (the list starts empty during the live scan, so the
    // cursor would otherwise stay unset until the user pressed a key).
    match app.ns_state.selected() {
        _ if ns_rows.is_empty() => app.ns_state.select(None),
        None => app.ns_state.select(Some(0)),
        Some(sel) if sel >= ns_rows.len() => app.ns_state.select(Some(0)),
        _ => {}
    }

    let selected = app.ns_state.selected();
    // Memory is only collected in advanced mode; hide the always-zero column in
    // basic mode rather than showing a misleading "0 B".
    let show_mem = app.advanced;
    let items: Vec<ListItem> = ns_rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            // On the selected row, render everything black so it reads cleanly on
            // the bright (cyan) highlight bar. Off-row, colour the columns for
            // scannability.
            let is_sel = ns_focused && selected == Some(i);
            let name_style = if is_sel {
                Style::default().fg(Color::Black)
            } else {
                // Leave ordinary text on the terminal's default foreground so
                // the UI remains readable on both dark and light themes.
                Style::default()
            };
            let (keys_c, mem_c) = if is_sel {
                (Color::Black, Color::Black)
            } else {
                (Color::Yellow, Color::Magenta)
            };
            let mut spans = vec![
                Span::styled(format!("{:<14}", truncate(&r.name, 14)), name_style),
                Span::styled(format!(" {:>10}", r.keys), Style::default().fg(keys_c)),
            ];
            if show_mem {
                spans.push(Span::styled(
                    format!("  {:>9}", format_size(r.bytes, DECIMAL)),
                    Style::default().fg(mem_c),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    // Highlight the selected namespace with a high-contrast cyan bar while this
    // pane is focused; keep a subtler marker when focus has moved to Types.
    let ns_highlight = if ns_focused {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border(ns_focused))
                .title(format!(" Namespaces ({}) ", ns_rows.len())),
        )
        .highlight_style(ns_highlight)
        .highlight_symbol(if ns_focused { "▶ " } else { "  " });
    f.render_stateful_widget(list, cols[0], &mut app.ns_state);

    // Right pane: type breakdown of the selected namespace.
    let selected_ns = app
        .ns_state
        .selected()
        .and_then(|i| ns_rows.get(i))
        .map(|r| r.name.clone());

    // In basic mode only counts are known, so show just Type/Keys/%. Advanced
    // mode adds the memory, data-type, and persist/expire columns.
    let header_cells = if show_mem {
        vec!["Type", "Keys", "%", "Total", "Avg", "Types", "P/E"]
    } else {
        vec!["Type", "Keys", "%"]
    };
    let header = Row::new(header_cells.into_iter().map(Cell::from).collect::<Vec<_>>())
        .style(Style::default().add_modifier(Modifier::BOLD));

    let (title, trows): (String, Vec<Row>) = match &selected_ns {
        Some(ns) => {
            let type_rows = filtered_types(stats, ns, &app.filter, app.sort);
            let total = stats.namespace_count(ns);
            let rows = type_rows
                .iter()
                .map(|t| {
                    let mut cells = vec![
                        Cell::from(truncate(&display_type(&t.name), 38)),
                        Cell::from(t.keys.to_string()),
                        Cell::from(pct(t.keys, total)),
                    ];
                    if show_mem {
                        cells.push(Cell::from(format_size(t.total_bytes, DECIMAL)));
                        cells.push(Cell::from(format_size(t.avg_bytes, DECIMAL)));
                        cells.push(Cell::from(truncate(&t.data_types, 16)));
                        cells.push(Cell::from(format!("{}/{}", t.persistent, t.expiring)));
                    }
                    Row::new(cells)
                })
                .collect();
            (format!(" {} — {} types ", ns, type_rows.len()), rows)
        }
        None => (" Types ".to_string(), Vec::new()),
    };

    let widths: &[Constraint] = if show_mem {
        &[
            Constraint::Min(20),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(16),
            Constraint::Length(7),
        ]
    } else {
        &[
            Constraint::Min(20),
            Constraint::Length(10),
            Constraint::Length(7),
        ]
    };
    // Only show the row cursor in the Types pane while it has focus.
    // (TableState is Copy, so this is a local copy, not a move out of `app`.)
    let mut type_state = app.type_state;
    if !types_focused {
        type_state.select(None);
    }
    let table = Table::new(trows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border(types_focused))
                .title(title),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, cols[1], &mut type_state);
}

fn draw_biggest(f: &mut Frame, area: Rect, app: &mut App, stats: &Stats) {
    let big = stats.biggest();

    if let Some(sel) = app.big_state.selected() {
        if sel >= big.len() {
            app.big_state
                .select(if big.is_empty() { None } else { Some(0) });
        }
    } else if !big.is_empty() {
        app.big_state.select(Some(0));
    }

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("Size"),
        Cell::from("Type"),
        Cell::from("TTL"),
        Cell::from("Key"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = big
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let ttl = ttl_label(b.ttl);
            Row::new(vec![
                Cell::from((i + 1).to_string()),
                Cell::from(format_size(b.bytes, DECIMAL)),
                Cell::from(b.data_type.clone().unwrap_or_else(|| "-".into())),
                Cell::from(ttl),
                Cell::from(display_key(&b.key)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Min(20),
    ];

    let title = if big.is_empty() {
        if app.advanced {
            " Biggest keys — none measured yet ".to_string()
        } else {
            " Biggest keys — basic scan; relaunch with advanced mode ".to_string()
        }
    } else {
        format!(" Biggest {} keys ", big.len())
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, area, &mut app.big_state);
}

/// The Persistent tab: how many keys have no TTL and how much memory they hold —
/// space expiry will never reclaim. A summary banner over a per-type table.
fn draw_persistent(f: &mut Frame, area: Rect, app: &mut App, stats: &Stats) {
    let rows_data = persistent_rows(stats);

    // Keep selection valid as data fills in.
    match app.persist_state.selected() {
        _ if rows_data.is_empty() => app.persist_state.select(None),
        None => app.persist_state.select(Some(0)),
        Some(sel) if sel >= rows_data.len() => app.persist_state.select(Some(0)),
        _ => {}
    }

    // Totals: persistent keys/bytes, and total measured bytes for a %.
    let persist_keys: u64 = rows_data.iter().map(|r| r.keys).sum();
    let persist_bytes: u64 = rows_data.iter().map(|r| r.bytes).sum();
    let measured_bytes: u64 = stats
        .namespaces
        .values()
        .flat_map(|t| t.values())
        .map(|s| s.total_bytes)
        .sum();
    let pct_mem = if measured_bytes > 0 {
        persist_bytes as f64 / measured_bytes as f64 * 100.0
    } else {
        0.0
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(area);

    // Banner.
    let banner = Line::from(vec![
        Span::styled(
            format!("{persist_keys} persistent keys"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  holding  "),
        Span::styled(
            format_size(persist_bytes, DECIMAL),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  ({pct_mem:.1}% of measured memory) — won't be freed by expiry"),
            Style::default().fg(Color::Gray),
        ),
    ]);
    f.render_widget(
        Paragraph::new(banner).block(Block::default().borders(Borders::BOTTOM)),
        layout[0],
    );

    // Table.
    let header = Row::new(
        ["Namespace", "Type", "Keys", "Memory"]
            .into_iter()
            .map(Cell::from)
            .collect::<Vec<_>>(),
    )
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = rows_data
        .iter()
        .map(|r| {
            Row::new(vec![
                Cell::from(truncate(&r.namespace, 16)),
                Cell::from(truncate(&display_type(&r.key_type), 40)),
                Cell::from(r.keys.to_string()),
                Cell::from(format_size(r.bytes, DECIMAL)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(16),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(11),
    ];

    let title = if rows_data.is_empty() {
        " Persistent keys — none found (every key has a TTL) ".to_string()
    } else {
        format!(" Persistent keys by type ({}) ", rows_data.len())
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(table, layout[1], &mut app.persist_state);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App, error: Option<&str>) {
    if let Some(err) = error {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(
                " ERROR: ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(err.to_string()),
        ]));
        f.render_widget(p, area);
        return;
    }

    if app.editing_filter {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{}_", app.filter),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   (Enter/Esc to apply)"),
        ]));
        f.render_widget(p, area);
        return;
    }

    let filt = if app.filter.is_empty() {
        String::new()
    } else {
        format!("  filter=\"{}\"", app.filter)
    };
    let can_drill = app.fetch_tx.is_some();
    let hint = match app.view {
        // In the Types pane (with a live connection), Enter drills into a type's
        // keys; otherwise Enter just steps Namespaces → Types.
        View::Browse if app.pane == Pane::Types && can_drill => format!(
            " ↑↓ move  Enter keys  ←/Esc back  Tab: view  s: sort [{}]  /: filter  q: quit{} ",
            app.sort.label(),
            filt,
        ),
        View::Browse => format!(
            " ↑↓ move  →/Enter types  Tab: view  s: sort [{}]  /: filter  q: quit{} ",
            app.sort.label(),
            filt,
        ),
        View::Biggest => format!(
            " ↑↓ move  Tab: view  s: sort [{}]  /: filter  q: quit{} ",
            app.sort.label(),
            filt,
        ),
        View::Persistent => format!(" ↑↓ move  Tab: view  /: filter  q: quit{filt} "),
    };
    let p = Paragraph::new(Line::from(vec![Span::styled(
        hint,
        Style::default().fg(Color::Gray),
    )]));
    f.render_widget(p, area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Render a TTL (seconds, or -1 persistent / -2 missing) for display.
fn ttl_label(ttl: Option<i64>) -> String {
    match ttl {
        Some(-1) => "persistent".to_string(),
        Some(t) if t >= 0 => format!("{t}s"),
        _ => "-".to_string(),
    }
}

/// The escaped Symfony tag-set marker as it appears in stored type/key names.
const TAG_MARKER: &str = "\\x01tags\\x01";

/// Humanize a *type name* for the TUI. The Symfony tag-index set is stored
/// canonically as `\x01tags\x01<name>`; show it as `🏷 <name>  (tag set)` so the
/// control bytes don't leak into the UI. Other type names pass through.
/// The underlying data (JSON/Markdown exports) keeps the canonical form.
fn display_type(name: &str) -> String {
    match name.strip_prefix(TAG_MARKER) {
        Some(tag) => format!("🏷 {tag}  (tag set)"),
        None => name.to_string(),
    }
}

/// Humanize a full *key* for the TUI (biggest-keys view): rewrite an embedded
/// `<namespace>:\x01tags\x01<name>` so the marker reads as a tag set.
fn display_key(key: &str) -> String {
    match key.split_once(TAG_MARKER) {
        Some((before, tag)) => format!("{before}🏷 tags » {tag}"),
        None => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{KeyObservation, Stats};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Render a frame into a TestBackend and return its text content.
    fn render_to_string(stats: &Stats, view: View) -> String {
        render_with_mode(stats, view, false)
    }

    fn render_with_mode(stats: &Stats, view: View, advanced: bool) -> String {
        let shared = Arc::new(Mutex::new(Shared {
            advanced,
            ..Default::default()
        }));
        let mut app = App::new(shared);
        app.view = view;
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, stats, true, None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn header_shows_scan_mode() {
        let s = sample();
        assert!(render_with_mode(&s, View::Browse, false).contains("basic"));
        assert!(render_with_mode(&s, View::Browse, true).contains("advanced"));
    }

    #[test]
    fn selected_namespace_row_is_readable_not_magenta() {
        // Regression: the selected row used to keep its per-column magenta/yellow
        // foreground over the dark-gray highlight, making the memory column
        // unreadable. The highlight must own the whole row (white on dark gray).
        let s = sample();
        let mut app = app_with(&s); // Namespaces pane focused, row 0 selected
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, &s, true, None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        // Find the highlighted row (the cyan selection bar in the left pane) and
        // check its text is black (readable), never the magenta column colour.
        let mut checked = false;
        for y in 0..buf.area.height {
            // The left namespace pane is the first ~46 columns.
            let row_is_highlighted =
                (0..46).any(|x| buf.cell((x, y)).map(|c| c.style().bg) == Some(Some(Color::Cyan)));
            if !row_is_highlighted {
                continue;
            }
            for x in 0..46 {
                if let Some(cell) = buf.cell((x, y)) {
                    assert_ne!(
                        cell.style().fg,
                        Some(Color::Magenta),
                        "selected row still has magenta text (unreadable)"
                    );
                }
            }
            checked = true;
        }
        assert!(checked, "no highlighted namespace row found");
    }

    #[test]
    fn header_shows_server_stats_when_available() {
        use crate::serverinfo::ServerInfo;
        let s = sample();
        let mut app = app_with(&s);
        let info = ServerInfo {
            redis_version: Some("7.2.4".into()),
            used_memory: Some(75_000_000),
            maxmemory: Some(100_000_000),
            evicted_keys: Some(42),
            mem_fragmentation_ratio: Some(1.80),
            db_keys: Some(1234),
            ..Default::default()
        };
        app.attach_server_info(Arc::new(Mutex::new(Some(info))));
        let out = render_app(&mut app, &s);
        assert!(out.contains("mem 75%"), "fullness missing: {out}");
        assert!(out.contains("keys 1234"), "db keys missing");
        assert!(out.contains("evicted 42"), "evictions missing");
        assert!(out.contains("frag 1.80"), "fragmentation missing");
        assert!(out.contains("v7.2.4"), "version missing");
    }

    #[test]
    fn header_shows_connecting_until_first_poll() {
        let s = sample();
        let mut app = app_with(&s);
        app.attach_server_info(Arc::new(Mutex::new(None)));
        assert!(render_app(&mut app, &s).contains("connecting"));
    }

    #[test]
    fn biggest_empty_hint_differs_by_mode() {
        // No biggest data + basic mode -> hint nudges toward advanced.
        let s = Stats::default();
        let basic = render_with_mode(&s, View::Biggest, false);
        assert!(basic.contains("basic scan"), "basic hint missing: {basic}");
        let adv = render_with_mode(&s, View::Biggest, true);
        assert!(
            !adv.contains("basic scan"),
            "advanced should not nudge: {adv}"
        );
    }

    #[test]
    fn browse_view_renders_memory_types_and_ttl_columns() {
        let s = sample();
        // These columns only exist in advanced mode.
        let out = render_with_mode(&s, View::Browse, true);
        // Namespaces appear.
        assert!(out.contains("alpha"), "namespace missing: {out}");
        // Memory column populated (humansize formats e.g. "6.05 kB" / "200 B").
        assert!(out.contains(" B") || out.contains("kB"), "no memory: {out}");
        // Data-type breakdown rendered for the selected namespace's types.
        assert!(out.contains("string:"), "no data types: {out}");
    }

    #[test]
    fn basic_mode_hides_memory_columns() {
        // In basic mode only counts are known; the namespace list and type table
        // must not show a misleading "0 B" memory column.
        let mut s = Stats::with_biggest_cap(0);
        let obs = KeyObservation {
            bytes: None,
            data_type: None,
            ttl: None,
        };
        s.record("ns:product-1", "ns", "product", &obs);
        let out = render_with_mode(&s, View::Browse, false);
        assert!(out.contains("product"), "type row missing: {out}");
        // No memory column: an unmeasured byte total would render as "0 B".
        // (Check the size value, not a bare " B", so the "Biggest keys" tab
        // label doesn't trigger a false positive.)
        assert!(
            !out.contains("0 B"),
            "basic mode shows a memory column: {out}"
        );
        assert!(
            !out.contains("kB"),
            "basic mode shows a memory column: {out}"
        );
    }

    #[test]
    fn first_namespace_auto_selected_once_scan_populates() {
        // The list starts empty during the live scan; the cursor must land on the
        // first namespace as soon as one appears (not stay unset).
        let empty = Stats::default();
        let mut app = app_with(&empty);
        // First frame: nothing scanned yet -> no selection.
        let _ = render_app(&mut app, &empty);
        assert_eq!(app.ns_state.selected(), None, "empty list -> no selection");

        // Next frame: a namespace has appeared -> auto-select row 0.
        let obs = KeyObservation {
            bytes: None,
            data_type: None,
            ttl: None,
        };
        let mut s = Stats::default();
        s.record("alpha:product-1", "alpha", "product", &obs);
        let _ = render_app(&mut app, &s);
        assert_eq!(
            app.ns_state.selected(),
            Some(0),
            "first namespace should be auto-selected"
        );
    }

    #[test]
    fn browse_view_humanizes_tag_set_type() {
        // One namespace, one tag-set type -> it's selected by default and its
        // type table must show the friendly label, not the control bytes.
        let obs = KeyObservation {
            bytes: Some(100),
            data_type: Some("set".into()),
            ttl: Some(-1),
        };
        let mut s = Stats::with_biggest_cap(0);
        s.record(
            "ns:\\x01tags\\x01product",
            "ns",
            "\\x01tags\\x01product",
            &obs,
        );
        let out = render_to_string(&s, View::Browse);
        assert!(out.contains("🏷"), "tag emoji missing: {out}");
        assert!(out.contains("(tag set)"), "tag label missing: {out}");
        assert!(!out.contains("\\x01"), "raw marker leaked: {out}");
    }

    #[test]
    fn biggest_view_lists_keys_when_memory_tracked() {
        let s = sample();
        let out = render_to_string(&s, View::Biggest);
        assert!(out.contains("Biggest"), "no biggest title: {out}");
        // The largest key (review-1, 5000 bytes) should be listed.
        assert!(out.contains("review-1"), "biggest key missing: {out}");
        // And NOT the empty-state hint, since memory was tracked.
        assert!(
            !out.contains("run with memory tracking"),
            "should not show empty hint: {out}"
        );
    }

    fn sample() -> Stats {
        let mut s = Stats::with_biggest_cap(10);
        let obs = |b: u64| KeyObservation {
            bytes: Some(b),
            data_type: Some("string".into()),
            ttl: Some(-1),
        };
        // ns "alpha": 3 keys of type "product", 1 of "review"
        s.record("alpha:product-1", "alpha", "product", &obs(100));
        s.record("alpha:product-2", "alpha", "product", &obs(900));
        s.record("alpha:product-3", "alpha", "product", &obs(50));
        s.record("alpha:review-1", "alpha", "review", &obs(5000));
        // ns "beta": 1 key
        s.record("beta:cart-1", "beta", "cart", &obs(200));
        s
    }

    #[test]
    fn filter_matches_namespace_substring_case_insensitive() {
        let s = sample();
        let rows = filtered_namespaces(&s, "AL", Sort::Count);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "alpha");
    }

    #[test]
    fn empty_filter_returns_all_namespaces() {
        let s = sample();
        assert_eq!(filtered_namespaces(&s, "", Sort::Count).len(), 2);
    }

    #[test]
    fn namespaces_sort_by_count_then_memory() {
        let s = sample();
        // alpha has 4 keys, beta has 1 -> alpha first by count.
        let by_count = filtered_namespaces(&s, "", Sort::Count);
        assert_eq!(by_count[0].name, "alpha");
        // By total memory alpha (6050) still beats beta (200).
        let by_mem = filtered_namespaces(&s, "", Sort::TotalMem);
        assert_eq!(by_mem[0].name, "alpha");
    }

    #[test]
    fn types_sort_toggles_between_count_and_avg() {
        let s = sample();
        // By count: product (3) before review (1).
        let by_count = filtered_types(&s, "alpha", "", Sort::Count);
        assert_eq!(by_count[0].name, "product");
        // By avg memory: review (5000) before product (avg ~350).
        let by_avg = filtered_types(&s, "alpha", "", Sort::AvgMem);
        assert_eq!(by_avg[0].name, "review");
    }

    #[test]
    fn truncate_keeps_short_strings_and_ellipsizes_long() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn display_type_humanizes_tag_sets_only() {
        assert_eq!(
            display_type("\\x01tags\\x01product"),
            "🏷 product  (tag set)"
        );
        // Non-tag types are unchanged.
        assert_eq!(display_type("product-detail-route"), "product-detail-route");
    }

    #[test]
    fn display_key_rewrites_embedded_tag_marker() {
        assert_eq!(
            display_key("WpDcVyo5fP:\\x01tags\\x01product-0f483ff8"),
            "WpDcVyo5fP:🏷 tags » product-0f483ff8"
        );
        assert_eq!(display_key("WpDcVyo5fP:product-1"), "WpDcVyo5fP:product-1");
    }

    #[test]
    fn filter_matches_humanized_tag_label() {
        // A namespace whose only type is a tag set.
        let obs = KeyObservation {
            bytes: Some(10),
            data_type: Some("set".into()),
            ttl: Some(-1),
        };
        let mut s = Stats::with_biggest_cap(0);
        s.record(
            "ns:\\x01tags\\x01product",
            "ns",
            "\\x01tags\\x01product",
            &obs,
        );
        // Typing "tag" finds it via the humanized "(tag set)" label, even
        // though the canonical name has no plain "tag" substring run.
        let rows = filtered_types(&s, "ns", "tag set", Sort::Count);
        assert_eq!(rows.len(), 1);
    }

    fn app_with(stats: &Stats) -> App {
        App::new(Arc::new(Mutex::new(Shared {
            stats: stats.clone(),
            ..Default::default()
        })))
    }

    /// App in advanced mode, where all tabs (Browse/Biggest/Persistent) exist.
    fn app_with_advanced(stats: &Stats) -> App {
        App::new(Arc::new(Mutex::new(Shared {
            stats: stats.clone(),
            advanced: true,
            ..Default::default()
        })))
    }

    fn press(app: &mut App, code: KeyCode, snapshot: &Stats) {
        handle_key(app, code, KeyModifiers::NONE, snapshot);
    }

    fn picker_app() -> (App, oneshot::Receiver<bool>) {
        let (tx, rx) = oneshot::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        (App::with_picker(shared, tx), rx)
    }

    #[test]
    fn picker_renders_inside_the_tui_frame() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, _rx) = oneshot::channel();
        let mut app = App::with_picker(shared, tx);
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, &Stats::default(), false, None))
            .unwrap();
        let out: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            out.contains("Choose scan depth"),
            "picker title missing: {out}"
        );
        assert!(out.contains("Basic"), "Basic option missing");
        assert!(out.contains("Advanced"), "Advanced option missing");
    }

    #[test]
    fn picker_description_uses_theme_foreground() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, _rx) = oneshot::channel();
        let mut app = App::with_picker(shared, tx);
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, &Stats::default(), false, None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        // Regression: explicit white text disappears on light terminal themes.
        // Descriptive picker copy should inherit the terminal foreground.
        let needle = "Key counts only";
        let mut checked = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect();
            let Some(start) = row.find(needle) else {
                continue;
            };
            for x in start as u16..start as u16 + needle.len() as u16 {
                let cell = buf.cell((x, y)).unwrap();
                assert_ne!(cell.style().fg, Some(Color::White));
                assert!(matches!(cell.style().fg, None | Some(Color::Reset)));
            }
            checked = true;
            break;
        }
        assert!(checked, "picker description text not found");
    }

    #[test]
    fn picker_enter_on_advanced_starts_advanced_scan() {
        let (mut app, rx) = picker_app();
        let s = Stats::default();
        press(&mut app, KeyCode::Down, &s); // highlight Advanced
        press(&mut app, KeyCode::Enter, &s);
        assert!(
            matches!(app.screen, Screen::Running),
            "should leave the picker"
        );
        assert!(app.advanced, "advanced flag must be set");
        assert_eq!(
            rx.blocking_recv().ok(),
            Some(true),
            "scan task gets advanced=true"
        );
    }

    #[test]
    fn picker_b_key_selects_basic() {
        let (mut app, rx) = picker_app();
        press(&mut app, KeyCode::Char('b'), &Stats::default());
        assert!(matches!(app.screen, Screen::Running));
        assert!(!app.advanced);
        assert_eq!(rx.blocking_recv().ok(), Some(false));
    }

    #[test]
    fn right_arrow_enters_types_pane_and_left_returns() {
        let s = sample();
        let mut app = app_with(&s);
        assert!(app.pane == Pane::Namespaces);

        press(&mut app, KeyCode::Right, &s);
        assert!(app.pane == Pane::Types, "Right should focus Types");
        assert_eq!(app.type_state.selected(), Some(0));

        press(&mut app, KeyCode::Left, &s);
        assert!(
            app.pane == Pane::Namespaces,
            "Left should return to Namespaces"
        );
    }

    #[test]
    fn tab_bar_shows_all_tabs_in_advanced() {
        let s = sample();
        let mut app = app_with_advanced(&s);
        let out = render_app(&mut app, &s);
        assert!(out.contains("Browse"), "Browse tab missing: {out}");
        assert!(out.contains("Biggest keys"), "Biggest tab missing");
        assert!(out.contains("Persistent"), "Persistent tab missing");
    }

    #[test]
    fn basic_mode_shows_only_browse_tab() {
        let s = sample();
        let mut app = app_with(&s); // basic
        let out = render_app(&mut app, &s);
        assert!(out.contains("Browse"));
        assert!(
            !out.contains("Biggest keys"),
            "advanced tabs leaked into basic"
        );
        assert!(
            !out.contains("Persistent"),
            "advanced tabs leaked into basic"
        );
    }

    #[test]
    fn tab_cycles_through_all_views_in_advanced() {
        let s = sample();
        let mut app = app_with_advanced(&s);
        assert!(app.view == View::Browse);
        press(&mut app, KeyCode::Tab, &s);
        assert!(app.view == View::Biggest, "Tab -> Biggest");
        press(&mut app, KeyCode::Tab, &s);
        assert!(app.view == View::Persistent, "Tab -> Persistent");
        press(&mut app, KeyCode::Tab, &s);
        assert!(app.view == View::Browse, "Tab wraps -> Browse");
        // Shift-Tab goes backwards.
        press(&mut app, KeyCode::BackTab, &s);
        assert!(app.view == View::Persistent, "Shift-Tab -> Persistent");
    }

    #[test]
    fn tab_is_noop_with_single_tab_in_basic() {
        let s = sample();
        let mut app = app_with(&s); // basic: only Browse
        press(&mut app, KeyCode::Tab, &s);
        assert!(app.view == View::Browse, "no other tab to switch to");
    }

    #[test]
    fn persistent_view_shows_summary_and_per_type_rows() {
        // Two persistent (no-TTL) keys totalling 1500 bytes, one expiring key.
        let mut s = Stats::default();
        let obs = |bytes: u64, ttl: i64| KeyObservation {
            bytes: Some(bytes),
            data_type: Some("string".into()),
            ttl: Some(ttl),
        };
        s.record("ns:config-1", "ns", "config", &obs(1000, -1));
        s.record("ns:config-2", "ns", "config", &obs(500, -1));
        s.record("ns:page-1", "ns", "page", &obs(9999, 60)); // expiring

        // Aggregation: only the persistent type appears, with its byte total.
        let rows = persistent_rows(&s);
        assert_eq!(rows.len(), 1, "only the persistent type");
        assert_eq!(rows[0].key_type, "config");
        assert_eq!(rows[0].keys, 2);
        assert_eq!(rows[0].bytes, 1500);

        let mut app = app_with_advanced(&s);
        app.view = View::Persistent;
        let out = render_app(&mut app, &s);
        assert!(
            out.contains("2 persistent keys"),
            "banner count missing: {out}"
        );
        assert!(out.contains("config"), "type row missing");
        // 1500 of 11499 measured bytes ≈ 13%.
        assert!(out.contains("won't be freed"), "explanation missing");
    }

    #[test]
    fn arrows_move_between_panes_in_browse() {
        // Pane focus now moves with the arrows (Tab is for views).
        let s = sample();
        let mut app = app_with(&s);
        assert!(app.pane == Pane::Namespaces);
        press(&mut app, KeyCode::Right, &s);
        assert!(app.pane == Pane::Types, "Right -> Types");
        press(&mut app, KeyCode::Left, &s);
        assert!(app.pane == Pane::Namespaces, "Left -> Namespaces");
    }

    #[test]
    fn arrows_move_type_cursor_when_types_pane_focused() {
        let s = sample();
        let mut app = app_with(&s);
        press(&mut app, KeyCode::Right, &s); // focus Types
        assert_eq!(app.type_state.selected(), Some(0));
        // alpha has 2 types (product, review) -> wraps within the pane.
        press(&mut app, KeyCode::Down, &s);
        assert_eq!(app.type_state.selected(), Some(1));
        press(&mut app, KeyCode::Down, &s);
        assert_eq!(app.type_state.selected(), Some(0), "should wrap");
        // The namespace cursor must NOT have moved while in the Types pane.
        assert_eq!(app.ns_state.selected(), Some(0));
    }

    #[test]
    fn changing_namespace_resets_type_cursor() {
        let s = sample();
        let mut app = app_with(&s);
        press(&mut app, KeyCode::Right, &s);
        press(&mut app, KeyCode::Down, &s); // type cursor -> 1
        assert_eq!(app.type_state.selected(), Some(1));
        press(&mut app, KeyCode::Left, &s); // back to namespaces
        press(&mut app, KeyCode::Down, &s); // alpha -> beta
        assert_eq!(app.type_state.selected(), Some(0), "type cursor resets");
    }

    #[test]
    fn cannot_focus_empty_types_pane() {
        let obs = KeyObservation {
            bytes: None,
            data_type: None,
            ttl: None,
        };
        // Namespace "alpha-store" with a type "widget". A filter of "alpha"
        // keeps the namespace visible but hides the type (it lacks "alpha"),
        // so the Types pane has no rows to land on.
        let mut s = Stats::with_biggest_cap(0);
        s.record("alpha-store:widget-1", "alpha-store", "widget", &obs);
        let mut app = app_with(&s);
        app.filter = "alpha".into();
        assert_eq!(filtered_namespaces(&s, "alpha", Sort::Count).len(), 1);
        assert_eq!(
            filtered_types(&s, "alpha-store", "alpha", Sort::Count).len(),
            0
        );
        press(&mut app, KeyCode::Right, &s);
        assert!(
            app.pane == Pane::Namespaces,
            "must not focus a pane with no rows"
        );
    }

    // ---- Drill-down (key list + value inspector) ----

    use crate::inspect::{Inbox, KeyList, KeyValue};

    /// Render whatever screen `app` is on into a buffer string.
    fn render_app(app: &mut App, stats: &Stats) -> String {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app, stats, true, None)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Push a result into the app's inbox and let poll_inbox pick it up.
    fn deliver(app: &mut App, set: impl FnOnce(&mut Inbox)) {
        let inbox = app.inbox.clone().unwrap();
        {
            let mut g = inbox.lock().unwrap();
            set(&mut g);
            g.generation += 1;
        }
        app.poll_inbox();
    }

    fn app_with_inbox(stats: &Stats) -> App {
        let mut app = app_with(stats);
        let inbox: crate::inspect::InboxHandle = Arc::new(Mutex::new(Inbox::default()));
        // No real fetcher; a dummy channel is enough for screen-transition tests.
        let (tx, _rx) = mpsc::channel(4);
        app.attach_inspector(tx, inbox);
        app
    }

    #[test]
    fn key_list_result_moves_to_keys_screen_and_renders() {
        let s = sample();
        let mut app = app_with_inbox(&s);
        deliver(&mut app, |inbox| {
            inbox.key_list = Some(KeyList {
                namespace: "alpha".into(),
                key_type: "product".into(),
                keys: vec!["alpha:product-1".into(), "alpha:product-2".into()],
                capped: false,
                error: None,
            });
        });
        assert!(matches!(app.screen, Screen::Keys));
        let out = render_app(&mut app, &s);
        assert!(out.contains("alpha:product-1"), "key missing: {out}");
        assert!(out.contains("2 keys"), "count missing: {out}");
    }

    #[test]
    fn value_result_renders_type_and_decompression_label() {
        let s = sample();
        let mut app = app_with_inbox(&s);
        // Be on the Keys screen first (poll only promotes to Value from Keys/Value).
        app.screen = Screen::Keys;
        deliver(&mut app, |inbox| {
            inbox.key_value = Some(KeyValue {
                key: "alpha:cached-page-x".into(),
                redis_type: "string".into(),
                ttl: Some(3600),
                size_bytes: Some(512),
                elements: None,
                body: "{\"hello\":\"world\"}".into(),
                hex: None,
                is_binary: false,
                encoding: Some(crate::value::Encoding::Gzip),
                stream_offset: None,
                error: None,
            });
        });
        assert!(matches!(app.screen, Screen::Value));
        let out = render_app(&mut app, &s);
        assert!(out.contains("string"), "type missing: {out}");
        assert!(
            out.contains("decompressed from gzip"),
            "encoding label missing: {out}"
        );
        assert!(out.contains("hello"), "body missing: {out}");
        assert!(out.contains("3600s"), "ttl missing: {out}");
        assert!(out.contains("expires "), "absolute expiry missing: {out}");
    }

    #[test]
    fn binary_value_defaults_to_hex_and_x_toggles_to_text() {
        let s = sample();
        let mut app = app_with_inbox(&s);
        app.screen = Screen::Keys;
        deliver(&mut app, |inbox| {
            inbox.key_value = Some(KeyValue {
                key: "alpha:blob".into(),
                redis_type: "string".into(),
                ttl: None,
                size_bytes: Some(4),
                elements: None,
                body: "lossy-text-here".into(),
                hex: Some("00000000  de ad be ef   ....".into()),
                is_binary: true,
                encoding: Some(crate::value::Encoding::Plain),
                stream_offset: None,
                error: None,
            });
        });
        // Binary -> defaults to the hex view.
        assert!(app.value_hex, "binary value should default to hex");
        let out = render_app(&mut app, &s);
        assert!(
            out.contains("de ad be ef"),
            "hex not shown by default: {out}"
        );

        // 'x' toggles to the text rendering.
        press(&mut app, KeyCode::Char('x'), &s);
        assert!(!app.value_hex);
        let out = render_app(&mut app, &s);
        assert!(
            out.contains("lossy-text-here"),
            "text not shown after toggle: {out}"
        );
    }

    #[test]
    fn esc_steps_back_out_of_drill_down() {
        let s = sample();
        let mut app = app_with_inbox(&s);
        app.screen = Screen::Value;
        app.key_value = Some(KeyValue {
            key: "k".into(),
            redis_type: "string".into(),
            ttl: None,
            size_bytes: None,
            elements: None,
            body: "x".into(),
            hex: None,
            is_binary: false,
            encoding: Some(crate::value::Encoding::Plain),
            stream_offset: None,
            error: None,
        });
        press(&mut app, KeyCode::Esc, &s);
        assert!(matches!(app.screen, Screen::Keys), "value Esc -> keys");
        press(&mut app, KeyCode::Esc, &s);
        assert!(matches!(app.screen, Screen::Running), "keys Esc -> running");
    }

    #[test]
    fn late_result_does_not_yank_user_out_of_navigation() {
        // If a key-list reply lands while we've navigated into a Value, it must
        // not pull us back to the Keys screen.
        let s = sample();
        let mut app = app_with_inbox(&s);
        app.screen = Screen::Value;
        deliver(&mut app, |inbox| {
            inbox.key_list = Some(KeyList {
                namespace: "alpha".into(),
                key_type: "product".into(),
                keys: vec!["alpha:product-1".into()],
                capped: false,
                error: None,
            });
        });
        assert!(matches!(app.screen, Screen::Value), "stayed on Value");
    }
}
