//! The `insights` command and its `tui` / `report` subcommands.
//!
//! Both subcommands share the same scan options (`ScanArgs`); they differ only
//! in how results are presented — live in the terminal (`tui`) or rendered to
//! stdout/a file in a chosen format (`report`).

use crate::report::{self, ReportOptions};
use crate::scanner::{self, ScanOptions};
use crate::stats::{self, Stats};
use crate::{connect, tui};
use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// How many biggest keys the TUI tracks by default. Bounded, so it's cheap.
const TUI_DEFAULT_BIGGEST: usize = 100;

#[derive(Subcommand, Debug)]
pub enum InsightsCmd {
    /// Interactive live viewer (scans in the background, fills in as it goes).
    Tui(ScanArgs),
    /// Render a one-shot report to stdout or a file.
    Report(ReportArgs),
}

/// Scan options shared by every insights subcommand.
#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    /// Only scan keys matching this glob pattern (server-side SCAN MATCH).
    #[arg(long)]
    pub pattern: Option<String>,

    /// COUNT hint for SCAN: keys per server-side step.
    #[arg(long, default_value_t = 1000)]
    pub count: usize,

    /// Stop after this many keys (0 = scan everything). Use for a fast sample.
    #[arg(long, default_value_t = 0)]
    pub limit: u64,

    /// Treat the first N characters as the namespace when a key has no ':'.
    #[arg(long, default_value_t = 10)]
    pub namespace_len: usize,

    /// Collect memory usage per key (MEMORY USAGE — extra round-trips).
    #[arg(long)]
    pub memory: bool,

    /// Collect Redis data type per key (TYPE — extra round-trips).
    #[arg(long)]
    pub types: bool,

    /// Collect TTL/expiry stats per key (TTL — extra round-trips).
    #[arg(long)]
    pub ttl: bool,

    /// Shortcut: enable --memory --types --ttl.
    #[arg(long)]
    pub full: bool,

    /// Track and report the N biggest keys by memory (implies --memory).
    #[arg(long, default_value_t = 0)]
    pub biggest: usize,

    /// TUI scan depth: `basic` (key counts only, fastest) or `advanced`
    /// (memory + type + TTL + biggest keys). If omitted, the TUI asks at
    /// startup. Ignored when explicit stat flags (--full/--memory/…) are given.
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
}

/// How much per-key enrichment the TUI scan collects.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Key counts only — one SCAN pass, no extra round-trips.
    Basic,
    /// Memory, data type, TTL, and the biggest-keys leaderboard.
    Advanced,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Table,
    Markdown,
    Json,
}

#[derive(Args, Debug)]
pub struct ReportArgs {
    #[command(flatten)]
    pub scan: ScanArgs,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,

    /// Write to this file instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,

    /// Only show the top N types per namespace (0 = all).
    #[arg(long, default_value_t = 25)]
    pub top: usize,
}

impl ScanArgs {
    /// True when the user explicitly asked for any per-key enrichment. In that
    /// case the TUI honors the flags and skips the basic/advanced prompt.
    fn has_explicit_stats(&self) -> bool {
        self.memory || self.types || self.ttl || self.full || self.biggest > 0
    }

    /// Resolve which enrichment stats to collect.
    ///
    /// `advanced` (the TUI advanced mode) forces the full set so every column
    /// and view is populated; basic mode leaves only what the flags request
    /// (typically nothing — a fast counts-only scan).
    fn resolve(&self, advanced: bool) -> (bool, bool, bool, usize) {
        let mut memory = self.memory || self.full || self.biggest > 0;
        let mut types = self.types || self.full;
        let mut ttl = self.ttl || self.full;
        let mut biggest = self.biggest;
        if advanced {
            memory = true;
            types = true;
            ttl = true;
            if biggest == 0 {
                biggest = TUI_DEFAULT_BIGGEST;
            }
        }
        (memory, types, ttl, biggest)
    }

    fn scan_options(&self, advanced: bool) -> ScanOptions {
        let (want_memory, want_types, want_ttl, biggest) = self.resolve(advanced);
        ScanOptions {
            count: self.count,
            namespace_len: self.namespace_len,
            limit: self.limit,
            want_memory,
            want_types,
            want_ttl,
            biggest,
        }
    }
}

/// Dispatch the `insights` command. `cmd` is `None` when the user typed bare
/// `insights` with no subcommand — we launch the TUI on a terminal, else hint.
pub async fn run(
    cmd: Option<InsightsCmd>,
    client: redis::Client,
    connect_timeout: u64,
) -> Result<()> {
    match cmd {
        Some(InsightsCmd::Tui(args)) => run_tui(args, client, connect_timeout).await,
        Some(InsightsCmd::Report(args)) => run_report(args, client, connect_timeout).await,
        None => {
            if std::io::stdout().is_terminal() {
                run_tui(ScanArgs::default_for_bare(), client, connect_timeout).await
            } else {
                eprintln!(
                    "No subcommand given. Run `insights report` for output, or \
                     `insights tui` for the interactive viewer."
                );
                Ok(())
            }
        }
    }
}

impl ScanArgs {
    /// Defaults for a bare `insights` invocation (the TUI prompts for depth).
    fn default_for_bare() -> ScanArgs {
        ScanArgs {
            pattern: None,
            count: 1000,
            limit: 0,
            namespace_len: 10,
            memory: false,
            types: false,
            ttl: false,
            full: false,
            biggest: 0,
            mode: None,
        }
    }
}

async fn run_report(args: ReportArgs, client: redis::Client, connect_timeout: u64) -> Result<()> {
    let scan_opts = args.scan.scan_options(false);
    let (show_memory, show_types, show_ttl, biggest) = args.scan.resolve(false);

    let mut conn = connect(client, connect_timeout).await?;

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {pos} keys scanned ({per_sec}) {msg}")
            .unwrap(),
    );
    let stats = scanner::scan(&mut conn, args.scan.pattern.as_deref(), scan_opts, |s| {
        pb.set_position(s.total_keys);
    })
    .await?;
    pb.finish_with_message("done");

    let report_opts = ReportOptions {
        show_memory,
        show_types,
        show_ttl,
        top: args.top,
        biggest,
    };

    match args.format {
        Format::Json => {
            let json = serde_json::to_string_pretty(&JsonReport {
                stats: &stats,
                biggest: stats.biggest(),
            })?;
            write_output(args.output.as_deref(), &json)?;
        }
        Format::Markdown => {
            let md = report::render_markdown(&stats, &report_opts);
            write_output(args.output.as_deref(), &md)?;
        }
        Format::Table => match args.output.as_deref() {
            // Terminal tables only make sense on stdout; for a file we fall back
            // to Markdown, which is the file-friendly tabular format.
            Some(path) => {
                let md = report::render_markdown(&stats, &report_opts);
                std::fs::write(path, md)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                eprintln!(
                    "Wrote Markdown report to {} (table format only prints to stdout)",
                    path.display()
                );
            }
            None => report::render(&stats, &report_opts),
        },
    }

    Ok(())
}

/// Launch the interactive TUI. The Redis scan runs in a background task that
/// publishes live snapshots; the synchronous UI loop runs on a blocking thread.
async fn run_tui(args: ScanArgs, client: redis::Client, connect_timeout: u64) -> Result<()> {
    // Decide scan depth. Explicit stat flags are honored exactly (no prompt).
    // An explicit --mode also skips the prompt. Otherwise the depth is chosen
    // *inside* the TUI via a one-shot the picker fires.
    let preset: Option<bool> = if args.has_explicit_stats() {
        Some(false) // honor exactly what the flags requested
    } else {
        args.mode.map(|m| m == Mode::Advanced)
    };

    let pattern = args.pattern.clone();

    // The picker (when shown) sends the chosen depth here; otherwise we send the
    // preset immediately so the scan starts right away.
    let (mode_tx, mode_rx) = tokio::sync::oneshot::channel::<bool>();

    let shared: tui::SharedHandle = Arc::new(Mutex::new(tui::Shared {
        advanced: preset.unwrap_or(false),
        ..Default::default()
    }));

    // Scan task: wait for the depth, then connect + scan with the right options.
    let fetch_client = client.clone(); // for the drill-down fetcher below
    let scan_shared = shared.clone();
    let scan_args = args.clone();
    let scan_task = tokio::spawn(async move {
        // Block until the user picks (or the preset is sent). If the UI is torn
        // down first the sender drops and we simply stop.
        let Ok(advanced) = mode_rx.await else { return };
        let scan_opts = scan_args.scan_options(advanced);

        let mut conn = match connect(client, connect_timeout).await {
            Ok(c) => c,
            Err(e) => {
                let mut g = scan_shared.lock().unwrap();
                g.error = Some(e.to_string());
                g.done = true;
                return;
            }
        };

        let result = scanner::scan(&mut conn, pattern.as_deref(), scan_opts, |s| {
            let mut g = scan_shared.lock().unwrap();
            g.stats = s.clone();
        })
        .await;

        let mut g = scan_shared.lock().unwrap();
        match result {
            Ok(s) => g.stats = s,
            Err(e) => g.error = Some(format!("scan failed: {e}")),
        }
        g.done = true;
    });

    // Drill-down fetcher: a separate task with its own (lazily-created)
    // connection, so the UI can list a type's keys and inspect values on demand.
    let inbox: crate::inspect::InboxHandle = Arc::new(Mutex::new(crate::inspect::Inbox::default()));
    let fetch_tx = crate::inspect::spawn(fetch_client, connect_timeout, inbox.clone());

    // Build the UI. With a preset depth, fire it now so the scan starts at once
    // and open straight on the running view. Otherwise hand the sender to the
    // in-TUI picker, which fires it once the user chooses.
    let ui_shared = shared.clone();
    let picker_tx = match preset {
        Some(advanced) => {
            let _ = mode_tx.send(advanced);
            None
        }
        None => Some(mode_tx),
    };

    let ui = tokio::task::spawn_blocking(move || {
        let mut app = match picker_tx {
            Some(tx) => tui::App::with_picker(ui_shared, tx),
            None => tui::App::new(ui_shared),
        };
        app.attach_inspector(fetch_tx, inbox);
        tui::run(&mut app)
    });

    let ui_result = ui.await.context("UI thread panicked")?;
    scan_task.abort();
    ui_result
}

#[derive(serde::Serialize)]
struct JsonReport<'a> {
    #[serde(flatten)]
    stats: &'a Stats,
    biggest: Vec<stats::BigKey>,
}

fn write_output(path: Option<&std::path::Path>, content: &str) -> Result<()> {
    match path {
        Some(p) => {
            std::fs::write(p, content)
                .with_context(|| format!("failed to write {}", p.display()))?;
            eprintln!("Wrote report to {}", p.display());
        }
        None => println!("{content}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_args() -> ScanArgs {
        ScanArgs::default_for_bare()
    }

    #[test]
    fn resolve_promotes_biggest_to_memory() {
        let mut a = scan_args();
        a.biggest = 20;
        let (mem, _types, _ttl, biggest) = a.resolve(false);
        assert!(mem, "--biggest must imply --memory");
        assert_eq!(biggest, 20);
    }

    #[test]
    fn resolve_full_enables_all_stats() {
        let mut a = scan_args();
        a.full = true;
        let (mem, types, ttl, _) = a.resolve(false);
        assert!(mem && types && ttl);
    }

    #[test]
    fn force_all_enables_everything_and_default_biggest() {
        let a = scan_args(); // no flags set
        let (mem, types, ttl, biggest) = a.resolve(true);
        assert!(mem && types && ttl, "TUI must force full stats");
        assert_eq!(biggest, TUI_DEFAULT_BIGGEST);
    }

    #[test]
    fn force_all_keeps_explicit_biggest() {
        let mut a = scan_args();
        a.biggest = 5;
        let (_, _, _, biggest) = a.resolve(true);
        assert_eq!(biggest, 5, "explicit --biggest must win over the default");
    }

    #[test]
    fn basic_mode_collects_nothing_extra() {
        // advanced=false with no flags -> a pure counts-only scan.
        let a = scan_args();
        let (mem, types, ttl, biggest) = a.resolve(false);
        assert!(!mem && !types && !ttl);
        assert_eq!(biggest, 0);
    }

    #[test]
    fn has_explicit_stats_detects_flags() {
        assert!(!scan_args().has_explicit_stats(), "no flags -> false");

        let mut a = scan_args();
        a.memory = true;
        assert!(a.has_explicit_stats());

        let mut a = scan_args();
        a.full = true;
        assert!(a.has_explicit_stats());

        let mut a = scan_args();
        a.biggest = 1;
        assert!(a.has_explicit_stats());

        // --mode alone is NOT an explicit stat flag (it selects, doesn't force).
        let mut a = scan_args();
        a.mode = Some(Mode::Advanced);
        assert!(!a.has_explicit_stats());
    }

    #[test]
    fn explicit_memory_flag_is_honored_not_forced_to_full() {
        // When the user passes only --memory, basic resolution keeps types/ttl
        // off — we must NOT silently upgrade them to a full scan.
        let mut a = scan_args();
        a.memory = true;
        let (mem, types, ttl, _) = a.resolve(false);
        assert!(mem && !types && !ttl);
    }
}
