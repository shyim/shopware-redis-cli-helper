package tui

import (
	"fmt"
	"sort"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/shopware-redis-cli-helper/internal"
)

// ==========================================================================
// Init
// ==========================================================================

func (m Model) Init() tea.Cmd {
	if m.presetAdvanced != nil {
		return func() tea.Msg { return scanConfigMsg{advanced: *m.presetAdvanced} }
	}
	m.screen = ScreenPicking
	return nil
}

// ==========================================================================
// Update
// ==========================================================================

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		return m, nil

	case tea.KeyMsg:
		return m.handleKey(msg)

	case scanConfigMsg:
		m.advanced = msg.advanced
		m.stats = internal.NewStats(m.scanOpts.Biggest)
		m.screen = ScreenRunning
		return m, tea.Batch(
			m.startScan(msg.advanced),
			m.startProgressPoller(),
			m.startInfoPoller(),
		)

	case scanProgressMsg:
		if msg.stats != nil {
			m.stats = msg.stats
		}
		m.done = msg.done
		m.err = msg.err
		m.totalScanned = msg.totalScanned
		if !m.done {
			return m, m.startProgressPoller()
		}
		return m, nil

	case serverInfoMsg:
		if msg.info != nil {
			m.info = msg.info
		}
		return m, nil

	case keyListResultMsg:
		m.loading = false
		if msg.list != nil {
			m.keyList = msg.list
			m.keyListCursor = 0
			if m.screen == ScreenRunning || m.screen == ScreenKeys {
				m.screen = ScreenKeys
			}
		}
		return m, nil

	case keyValueResultMsg:
		m.loading = false
		if msg.kv != nil {
			m.keyValue = msg.kv
			m.valueHex = msg.kv.IsBinary && msg.kv.Hex != ""
			m.valueScroll = 0
			if m.screen == ScreenKeys || m.screen == ScreenValue {
				m.screen = ScreenValue
			}
		}
		return m, nil

	case tickMsg:
		return m, tea.Batch(m.fetchServerInfo(), m.startProgressPoller())
	}

	return m, nil
}

// ==========================================================================
// Key handling
// ==========================================================================

func (m Model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	keyStr := msg.String()

	if keyStr == "q" || keyStr == "ctrl+c" {
		m.Cleanup()
		return m, tea.Quit
	}

	switch m.screen {
	case ScreenPicking:
		return m.handlePickerKey(keyStr)
	case ScreenRunning:
		return m.handleRunningKey(msg)
	case ScreenKeys:
		return m.handleKeysKey(keyStr)
	case ScreenValue:
		return m.handleValueKey(keyStr)
	}
	return m, nil
}

func (m Model) handlePickerKey(keyStr string) (tea.Model, tea.Cmd) {
	switch keyStr {
	case "left", "h", "up", "k":
		m.pickerSelected = 0
	case "right", "l", "down", "j":
		m.pickerSelected = 1
	case "tab":
		m.pickerSelected = 1 - m.pickerSelected
	case "b":
		return m, func() tea.Msg { return scanConfigMsg{advanced: false} }
	case "a":
		return m, func() tea.Msg { return scanConfigMsg{advanced: true} }
	case "enter":
		return m, func() tea.Msg { return scanConfigMsg{advanced: m.pickerSelected == 1} }
	}
	return m, nil
}

func (m Model) handleRunningKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	// Filter editing
	if m.editingFilter {
		switch msg.String() {
		case "esc", "enter":
			m.editingFilter = false
			m.nsCursor = 0
			m.typeCursor = 0
			m.pane = PaneNamespaces
			return m, nil
		case "backspace":
			if len(m.filter) > 0 {
				r := []rune(m.filter)
				m.filter = string(r[:len(r)-1])
			}
			m.nsCursor = 0
			m.typeCursor = 0
			m.pane = PaneNamespaces
			return m, nil
		default:
			if len(msg.Key().Text) == 1 && msg.Key().Text[0] >= 32 {
				m.filter += msg.Key().Text
			}
			m.nsCursor = 0
			m.typeCursor = 0
			m.pane = PaneNamespaces
			return m, nil
		}
	}

	return m.handleRunningKeyStr(msg.String())
}

func (m Model) handleRunningKeyStr(keyStr string) (tea.Model, tea.Cmd) {
	switch keyStr {
	case "/":
		m.editingFilter = true
		return m, nil
	case "s":
		m.sort = m.sort.Next()
		return m, nil
	case "tab":
		m.view = cycleView(m.view, m.advanced, true)
		return m, nil
	case "shift+tab":
		m.view = cycleView(m.view, m.advanced, false)
		return m, nil
	case "b":
		m.view = cycleView(m.view, m.advanced, true)
		return m, nil
	case "right", "l":
		if m.view == ViewBrowse && m.canFocusTypes() {
			m.pane = PaneTypes
		}
		return m, nil
	case "left", "h", "esc":
		m.pane = PaneNamespaces
		return m, nil
	case "enter":
		if m.view == ViewBrowse {
			if m.pane == PaneNamespaces && m.canFocusTypes() {
				m.pane = PaneTypes
			} else if m.pane == PaneTypes {
				return m, m.drillIntoType()
			}
		}
		return m, nil
	case "up", "k":
		m.moveSelection(-1)
		return m, nil
	case "down", "j":
		m.moveSelection(1)
		return m, nil
	}
	return m, nil
}

func (m Model) handleKeysKey(keyStr string) (tea.Model, tea.Cmd) {
	switch keyStr {
	case "esc", "left", "h":
		m.screen = ScreenRunning
		return m, nil
	case "up", "k":
		if m.keyList != nil && len(m.keyList.Keys) > 0 {
			m.keyListCursor = (m.keyListCursor - 1 + len(m.keyList.Keys)) % len(m.keyList.Keys)
		}
		return m, nil
	case "down", "j":
		if m.keyList != nil && len(m.keyList.Keys) > 0 {
			m.keyListCursor = (m.keyListCursor + 1) % len(m.keyList.Keys)
		}
		return m, nil
	case "enter", "right", "l":
		return m, m.drillIntoKey()
	}
	return m, nil
}

func (m Model) handleValueKey(keyStr string) (tea.Model, tea.Cmd) {
	headerH := 1
	footerH := 1
	metaH := 6
	bodyH := m.height - headerH - footerH - metaH
	if bodyH < 3 {
		bodyH = 3
	}
	visibleLines := bodyH - 2
	if visibleLines < 1 {
		visibleLines = 1
	}

	switch keyStr {
	case "esc", "left", "h":
		m.screen = ScreenKeys
		m.keyValue = nil
		return m, nil
	case "x":
		if m.keyValue != nil && m.keyValue.Hex != "" {
			m.valueHex = !m.valueHex
			m.valueScroll = 0
		}
		return m, nil
	case "down", "j":
		if m.keyValue != nil {
			var bodyContent string
			if m.valueHex && m.keyValue.Hex != "" {
				bodyContent = m.keyValue.Hex
			} else {
				bodyContent = m.keyValue.Body
			}
			linesCount := len(strings.Split(bodyContent, "\n"))
			maxScroll := linesCount - visibleLines
			if maxScroll < 0 {
				maxScroll = 0
			}
			if m.valueScroll < maxScroll {
				m.valueScroll++
			}
		}
		return m, nil
	case "up", "k":
		if m.valueScroll > 0 {
			m.valueScroll--
		}
		return m, nil
	case "pgdown":
		if m.keyValue != nil {
			var bodyContent string
			if m.valueHex && m.keyValue.Hex != "" {
				bodyContent = m.keyValue.Hex
			} else {
				bodyContent = m.keyValue.Body
			}
			linesCount := len(strings.Split(bodyContent, "\n"))
			maxScroll := linesCount - visibleLines
			if maxScroll < 0 {
				maxScroll = 0
			}
			m.valueScroll += 20
			if m.valueScroll > maxScroll {
				m.valueScroll = maxScroll
			}
		}
		return m, nil
	case "pgup":
		if m.valueScroll >= 20 {
			m.valueScroll -= 20
		} else {
			m.valueScroll = 0
		}
		return m, nil
	}
	return m, nil
}

// ==========================================================================
// Commands
// ==========================================================================

func (m *Model) startScan(advanced bool) tea.Cmd {
	opts := m.scanOpts
	if advanced {
		opts.WantMemory = true
		opts.WantTypes = true
		opts.WantTTL = true
		if opts.Biggest == 0 {
			opts.Biggest = 100
		}
	}

	return func() tea.Msg {
		client, err := internal.ConnectRedis(m.redisURL, m.connectTimeout)
		if err != nil {
			m.progressCh <- scanProgressMsg{err: err.Error(), done: true}
			return nil
		}

		go func() {
			defer func() { _ = client.Close() }()

			_, err := internal.Scan(m.ctx, client, m.pattern, opts, func(s *internal.Stats) {
				clone := cloneStats(s)
				select {
				case m.progressCh <- scanProgressMsg{stats: clone, totalScanned: s.TotalKeys}:
				default:
				}
			})

			if err != nil {
				m.progressCh <- scanProgressMsg{err: fmt.Sprintf("scan failed: %v", err), done: true}
			} else {
				// Final snapshot is handled by the last progress callback, just mark as done
				m.progressCh <- scanProgressMsg{done: true, totalScanned: m.totalScanned}
			}
		}()

		return nil
	}
}

func cloneStats(s *internal.Stats) *internal.Stats {
	clone := internal.NewStats(0)
	clone.TotalKeys = s.TotalKeys
	for ns, types := range s.Namespaces {
		clone.Namespaces[ns] = make(map[string]*internal.TypeStat)
		for t, stat := range types {
			cs := &internal.TypeStat{
				Count:           stat.Count,
				TotalBytes:      stat.TotalBytes,
				Measured:        stat.Measured,
				Persistent:      stat.Persistent,
				Expiring:        stat.Expiring,
				PersistentBytes: stat.PersistentBytes,
				DataTypes:       make(map[string]int64),
			}
			for dt, c := range stat.DataTypes {
				cs.DataTypes[dt] = c
			}
			clone.Namespaces[ns][t] = cs
		}
	}
	return clone
}

func (m *Model) startProgressPoller() tea.Cmd {
	return func() tea.Msg {
		select {
		case msg := <-m.progressCh:
			return msg
		case <-time.After(100 * time.Millisecond):
			return nil
		}
	}
}

func (m *Model) startInfoPoller() tea.Cmd {
	return tea.Batch(
		m.fetchServerInfo(),
		tea.Tick(pollInterval, func(t time.Time) tea.Msg {
			return tickMsg(t)
		}),
	)
}

func (m *Model) fetchServerInfo() tea.Cmd {
	return func() tea.Msg {
		if m.client == nil {
			return nil
		}
		raw, err := m.client.Info(m.ctx).Result()
		if err != nil {
			return nil
		}
		info := internal.ParseInfo(raw)
		if dbSize, err := m.client.DBSize(m.ctx).Result(); err == nil {
			info.DBKeys = dbSize
		}
		return serverInfoMsg{info: info}
	}
}

func (m *Model) drillIntoType() tea.Cmd {
	nsRows := m.filteredNamespaces()
	if m.nsCursor >= len(nsRows) {
		return nil
	}
	ns := nsRows[m.nsCursor].Name
	typeRows := m.filteredTypeRows()
	if m.typeCursor >= len(typeRows) {
		return nil
	}
	t := typeRows[m.typeCursor].Name

	m.loading = true
	return func() tea.Msg {
		list := internal.ListKeys(m.ctx, m.client, ns, t)
		return keyListResultMsg{list: list}
	}
}

func (m *Model) drillIntoKey() tea.Cmd {
	if m.keyList == nil || m.keyListCursor >= len(m.keyList.Keys) {
		return nil
	}
	key := m.keyList.Keys[m.keyListCursor]
	m.loading = true
	return func() tea.Msg {
		kv := internal.GetValue(m.ctx, m.client, key, internal.ValueDisplayCap)
		return keyValueResultMsg{kv: kv}
	}
}

// ==========================================================================
// Selection & cursor helpers
// ==========================================================================

func (m *Model) canFocusTypes() bool {
	return len(m.filteredTypeRows()) > 0
}

func (m *Model) moveSelection(delta int) {
	switch m.view {
	case ViewBrowse:
		switch m.pane {
		case PaneNamespaces:
			rows := m.filteredNamespaces()
			if len(rows) == 0 {
				return
			}
			m.nsCursor = (m.nsCursor + delta) % len(rows)
			if m.nsCursor < 0 {
				m.nsCursor += len(rows)
			}
			m.typeCursor = 0
		case PaneTypes:
			rows := m.filteredTypeRows()
			if len(rows) == 0 {
				return
			}
			m.typeCursor = (m.typeCursor + delta) % len(rows)
			if m.typeCursor < 0 {
				m.typeCursor += len(rows)
			}
		}
	case ViewBiggest:
		big := m.stats.Biggest()
		if len(big) == 0 {
			return
		}
		m.bigCursor = (m.bigCursor + delta) % len(big)
		if m.bigCursor < 0 {
			m.bigCursor += len(big)
		}
	case ViewPersistent:
		rows := internal.PersistentRows(m.stats)
		if len(rows) == 0 {
			return
		}
		m.persistCursor = (m.persistCursor + delta) % len(rows)
		if m.persistCursor < 0 {
			m.persistCursor += len(rows)
		}
	}
}

// ==========================================================================
// Data helpers
// ==========================================================================

func (m *Model) filteredNamespaces() []NsRow {
	f := strings.ToLower(m.filter)
	var rows []NsRow
	for ns := range m.stats.Namespaces {
		if f != "" && !strings.Contains(strings.ToLower(ns), f) {
			continue
		}
		rows = append(rows, NsRow{
			Name:  ns,
			Keys:  m.stats.NamespaceCount(ns),
			Bytes: m.stats.NamespaceBytes(ns),
		})
	}
	sortNsRows(rows, m.sort)
	return rows
}

func (m *Model) filteredTypeRows() []TypeRow {
	nsRows := m.filteredNamespaces()
	if m.nsCursor >= len(nsRows) {
		return nil
	}
	ns := nsRows[m.nsCursor].Name
	types := m.stats.Namespaces[ns]
	if types == nil {
		return nil
	}
	f := strings.ToLower(m.filter)
	var rows []TypeRow
	for name, stat := range types {
		displayName := internal.DisplayType(name)
		if f != "" && !strings.Contains(strings.ToLower(name), f) && !strings.Contains(strings.ToLower(displayName), f) {
			continue
		}
		rows = append(rows, TypeRow{
			Name:       name,
			Keys:       stat.Count,
			TotalBytes: stat.TotalBytes,
			AvgBytes:   stat.AvgBytes(),
			DataTypes:  dataTypesStr(stat),
			Persistent: stat.Persistent,
			Expiring:   stat.Expiring,
		})
	}
	sortTypeRows(rows, m.sort)
	return rows
}

func dataTypesStr(stat *internal.TypeStat) string {
	type pair struct {
		name  string
		count int64
	}
	var pairs []pair
	for k, v := range stat.DataTypes {
		pairs = append(pairs, pair{k, v})
	}
	sort.Slice(pairs, func(i, j int) bool {
		if pairs[i].count == pairs[j].count {
			return pairs[i].name < pairs[j].name
		}
		return pairs[i].count > pairs[j].count
	})
	var parts []string
	for _, p := range pairs {
		parts = append(parts, fmt.Sprintf("%s:%d", p.name, p.count))
	}
	return strings.Join(parts, " ")
}

func sortNsRows(rows []NsRow, sortBy Sort) {
	sort.Slice(rows, func(i, j int) bool {
		switch sortBy {
		case SortCount:
			if rows[i].Keys == rows[j].Keys {
				return rows[i].Name < rows[j].Name
			}
			return rows[i].Keys > rows[j].Keys
		case SortTotalMem, SortAvgMem:
			if rows[i].Bytes == rows[j].Bytes {
				return rows[i].Name < rows[j].Name
			}
			return rows[i].Bytes > rows[j].Bytes
		}
		if rows[i].Keys == rows[j].Keys {
			return rows[i].Name < rows[j].Name
		}
		return rows[i].Keys > rows[j].Keys
	})
}

func sortTypeRows(rows []TypeRow, sortBy Sort) {
	sort.Slice(rows, func(i, j int) bool {
		switch sortBy {
		case SortCount:
			if rows[i].Keys == rows[j].Keys {
				return rows[i].Name < rows[j].Name
			}
			return rows[i].Keys > rows[j].Keys
		case SortTotalMem:
			if rows[i].TotalBytes == rows[j].TotalBytes {
				return rows[i].Name < rows[j].Name
			}
			return rows[i].TotalBytes > rows[j].TotalBytes
		case SortAvgMem:
			if rows[i].AvgBytes == rows[j].AvgBytes {
				return rows[i].Name < rows[j].Name
			}
			return rows[i].AvgBytes > rows[j].AvgBytes
		}
		if rows[i].Keys == rows[j].Keys {
			return rows[i].Name < rows[j].Name
		}
		return rows[i].Keys > rows[j].Keys
	})
}
