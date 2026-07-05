package tui

import (
	"context"
	"time"

	"charm.land/bubbles/v2/key"
	tea "charm.land/bubbletea/v2"
	"github.com/redis/go-redis/v9"

	"github.com/shopware-redis-cli-helper/internal"
)

// ==========================================================================
// Types & Enums
// ==========================================================================

// Screen represents which view is currently active.
type Screen int

const (
	ScreenPicking Screen = iota
	ScreenRunning
	ScreenKeys
	ScreenValue
)

// View represents top-level tabs.
type View int

const (
	ViewBrowse View = iota
	ViewBiggest
	ViewPersistent
)

func (v View) Title() string {
	switch v {
	case ViewBrowse:
		return "Browse"
	case ViewBiggest:
		return "Biggest keys"
	case ViewPersistent:
		return "Persistent"
	}
	return ""
}

func availableViews(advanced bool) []View {
	if advanced {
		return []View{ViewBrowse, ViewBiggest, ViewPersistent}
	}
	return []View{ViewBrowse}
}

func cycleView(v View, advanced bool, forward bool) View {
	tabs := availableViews(advanced)
	for i, tv := range tabs {
		if tv == v {
			delta := 1
			if !forward {
				delta = -1
			}
			next := (i + delta) % len(tabs)
			if next < 0 {
				next += len(tabs)
			}
			return tabs[next]
		}
	}
	return ViewBrowse
}

// Pane represents focus side in Browse view.
type Pane int

const (
	PaneNamespaces Pane = iota
	PaneTypes
)

// Sort order for tables.
type Sort int

const (
	SortCount Sort = iota
	SortTotalMem
	SortAvgMem
)

func (s Sort) Label() string {
	switch s {
	case SortCount:
		return "count"
	case SortTotalMem:
		return "total mem"
	case SortAvgMem:
		return "avg mem"
	}
	return ""
}

func (s Sort) Next() Sort {
	switch s {
	case SortCount:
		return SortTotalMem
	case SortTotalMem:
		return SortAvgMem
	case SortAvgMem:
		return SortCount
	}
	return SortCount
}

// Display rows
type NsRow struct {
	Name  string
	Keys  int64
	Bytes int64
}

type TypeRow struct {
	Name       string
	Keys       int64
	TotalBytes int64
	AvgBytes   int64
	DataTypes  string
	Persistent int64
	Expiring   int64
}

// ==========================================================================
// Messages
// ==========================================================================

type scanProgressMsg struct {
	stats        *internal.Stats
	done         bool
	err          string
	totalScanned int64
}

type serverInfoMsg struct {
	info *internal.ServerInfo
}

type keyListResultMsg struct {
	list *internal.KeyList
}

type keyValueResultMsg struct {
	kv *internal.KeyValue
}

type scanConfigMsg struct {
	advanced bool
}

type tickMsg time.Time

const pollInterval = 2 * time.Second

// ==========================================================================
// Key Bindings
// ==========================================================================

type keyMap struct {
	Up      key.Binding
	Down    key.Binding
	Left    key.Binding
	Right   key.Binding
	Enter   key.Binding
	Tab     key.Binding
	BackTab key.Binding
	Quit    key.Binding
	Filter  key.Binding
	SortKey key.Binding
	Hex     key.Binding
	Esc     key.Binding
}

func defaultKeyMap() keyMap {
	return keyMap{
		Up:      key.NewBinding(key.WithKeys("up", "k"), key.WithHelp("↑/k", "up")),
		Down:    key.NewBinding(key.WithKeys("down", "j"), key.WithHelp("↓/j", "down")),
		Left:    key.NewBinding(key.WithKeys("left", "h"), key.WithHelp("←/h", "back")),
		Right:   key.NewBinding(key.WithKeys("right", "l"), key.WithHelp("→/l", "enter")),
		Enter:   key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "select")),
		Tab:     key.NewBinding(key.WithKeys("tab"), key.WithHelp("tab", "next view")),
		BackTab: key.NewBinding(key.WithKeys("shift+tab"), key.WithHelp("shift+tab", "prev view")),
		Quit:    key.NewBinding(key.WithKeys("q", "ctrl+c"), key.WithHelp("q", "quit")),
		Filter:  key.NewBinding(key.WithKeys("/"), key.WithHelp("/", "filter")),
		SortKey: key.NewBinding(key.WithKeys("s"), key.WithHelp("s", "sort")),
		Hex:     key.NewBinding(key.WithKeys("x"), key.WithHelp("x", "hex toggle")),
		Esc:     key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "back")),
	}
}

// ==========================================================================
// Model
// ==========================================================================

type Model struct {
	// Config
	redisURL       string
	connectTimeout int
	pattern        string
	scanOpts       internal.ScanOptions
	presetAdvanced *bool // nil = show picker

	// Redis
	client *redis.Client

	// State
	stats        *internal.Stats
	done         bool
	err          string
	advanced     bool
	totalScanned int64

	// Channels
	progressCh chan tea.Msg

	// Server info
	info *internal.ServerInfo

	// Context
	ctx    context.Context
	cancel context.CancelFunc

	// Screen
	screen Screen

	// Picker
	pickerSelected int

	// Browse
	view          View
	pane          Pane
	sort          Sort
	nsCursor      int
	typeCursor    int
	bigCursor     int
	persistCursor int
	filter        string
	editingFilter bool

	// Drill-down: key list
	keyList       *internal.KeyList
	keyListCursor int
	loading       bool

	// Drill-down: value
	keyValue    *internal.KeyValue
	valueScroll int
	valueHex    bool

	// Dimensions
	width  int
	height int

	// Keys
	keys keyMap
}

func NewModel(redisURL string, connectTimeout int, pattern string, scanOpts internal.ScanOptions, presetAdvanced *bool) Model {
	ctx, cancel := context.WithCancel(context.Background())
	return Model{
		redisURL:       redisURL,
		connectTimeout: connectTimeout,
		pattern:        pattern,
		scanOpts:       scanOpts,
		presetAdvanced: presetAdvanced,
		stats:          internal.NewStats(scanOpts.Biggest),
		screen:         ScreenPicking,
		view:           ViewBrowse,
		pane:           PaneNamespaces,
		sort:           SortCount,
		progressCh:     make(chan tea.Msg, 128),
		ctx:            ctx,
		cancel:         cancel,
		keys:           defaultKeyMap(),
	}
}

func (m *Model) Cleanup() {
	m.cancel()
}

func (m *Model) SetClient(client *redis.Client) {
	m.client = client
}
