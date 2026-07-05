package tui

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	lipgloss "charm.land/lipgloss/v2"
	"charm.land/lipgloss/v2/compat"
	"github.com/dustin/go-humanize"

	"github.com/shopware-redis-cli-helper/internal"
)

// ==========================================================================
// Top-level View
// ==========================================================================

func (m Model) View() tea.View {
	v := tea.NewView(m.render())
	v.AltScreen = true
	return v
}

func (m Model) render() string {
	if m.width < 50 || m.height < 10 {
		return textMuted.Render("Terminal too small (need at least 50x10)")
	}
	spinnerIdx++

	switch m.screen {
	case ScreenPicking:
		return m.renderPicker()
	case ScreenKeys:
		return m.renderKeyList()
	case ScreenValue:
		return m.renderValue()
	default:
		return m.renderMain()
	}
}

// ==========================================================================
// Picker screen
// ==========================================================================

func (m Model) renderPicker() string {
	title := primaryBold.Render("  Shopware Redis Insights  ")
	subtitle := textMuted.Render("Select database scanning depth to begin")

	// Card for Basic Mode
	basicActive := m.pickerSelected == 0
	basicTitle := " ⚡ Basic Mode "
	if basicActive {
		basicTitle = lipgloss.NewStyle().Foreground(colSelFg).Background(colPrimary).Bold(true).Render(basicTitle)
	} else {
		basicTitle = lipgloss.NewStyle().Foreground(colPrimary).Bold(true).Render(basicTitle)
	}
	basicDesc := "• Key counts & namespace grouping only\n• Fast, single SCAN pass\n• Low memory & CPU overhead on Redis"
	basicCard := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(condColor(basicActive, colPrimary, colBorder)).
		Padding(1, 2).
		Width(28).
		Render(basicTitle + "\n\n" + textDim.Render(basicDesc))

	// Card for Advanced Mode
	advActive := m.pickerSelected == 1
	advTitle := " 🔍 Advanced Mode "
	if advActive {
		advTitle = lipgloss.NewStyle().Foreground(colSelFg).Background(colAccent).Bold(true).Render(advTitle)
	} else {
		advTitle = lipgloss.NewStyle().Foreground(colAccent).Bold(true).Render(advTitle)
	}
	advDesc := "• Full memory, TTL & data type stats\n• Identifies biggest keys\n• Performs auxiliary queries (slow on huge DBs)"
	advCard := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(condColor(advActive, colAccent, colBorder)).
		Padding(1, 2).
		Width(28).
		Render(advTitle + "\n\n" + textDim.Render(advDesc))

	// Join cards horizontally
	cards := lipgloss.JoinHorizontal(lipgloss.Top, basicCard, "  ", advCard)

	help := textMuted.Render("  ←/→/tab to choose  ·  enter to confirm  ·  q to quit")

	pickerContent := lipgloss.JoinVertical(lipgloss.Center,
		title,
		subtitle,
		"",
		cards,
		"",
		help,
	)

	boxed := lipgloss.NewStyle().
		Border(lipgloss.DoubleBorder()).
		BorderForeground(colPrimary).
		Padding(2, 4).
		Render(pickerContent)

	return lipgloss.Place(m.width, m.height, lipgloss.Center, lipgloss.Center, boxed)
}

// ==========================================================================
// Main dashboard
// ==========================================================================

func (m Model) renderMain() string {
	// Layout: header (2) + tabs (1) + spacing (1) + body (fill) + footer (1)
	headerH := 2
	footerH := 1
	tabsH := 2 // gives tabs a blank line after it for breathing room
	bodyH := m.height - headerH - footerH - tabsH
	if bodyH < 5 {
		bodyH = 5
	}

	return lipgloss.JoinVertical(lipgloss.Top,
		m.renderHeader(headerH),
		m.renderTabs(),
		"", // Blank line spacer
		m.renderBody(bodyH),
		m.renderFooter(),
	)
}

// --- Header ---

func (m Model) renderHeader(h int) string {
	statusLine := m.renderStatusLine()
	infoLine := m.renderServerInfo()

	content := lipgloss.JoinVertical(lipgloss.Top, statusLine, infoLine)
	return lipgloss.NewStyle().Padding(0, 1).Render(content)
}

func (m Model) renderStatusLine() string {
	var status string
	var style lipgloss.Style

	switch {
	case m.err != "":
		status = "✗ " + trunc(m.err, m.width-20)
		style = dangerBold
	case !m.done && m.totalScanned == 0:
		status = spinnerGlyph() + " Connecting to Redis…"
		style = warning
	case m.done:
		status = "✓ Scan complete · " + humanize.Comma(m.totalScanned) + " keys scanned"
		style = successBold
	default:
		bar := m.renderCompactProgressBar()
		status = spinnerGlyph() + " Scanning" + bar + " · " + humanize.Comma(m.totalScanned) + " keys"
		style = primary
	}

	mode := "BASIC"
	if m.advanced {
		mode = "ADVANCED"
	}
	modeBadge := primaryBold.Render("[" + mode + "]")

	leftSide := style.Render(status)
	rightSide := modeBadge

	paddingCount := m.width - 2 - lipgloss.Width(leftSide) - lipgloss.Width(rightSide)
	if paddingCount < 0 {
		paddingCount = 0
	}

	return leftSide + strings.Repeat(" ", paddingCount) + rightSide
}

func (m Model) renderCompactProgressBar() string {
	if m.done {
		return ""
	}
	barW := 10
	ratio := 0.0
	if m.totalScanned > 0 {
		ratio = float64(m.totalScanned%100) / 100.0
	}
	filled := int(float64(barW) * ratio)
	if filled > barW {
		filled = barW
	}
	return " [" + primary.Render(strings.Repeat("█", filled)) +
		textMuted.Render(strings.Repeat("░", barW-filled)) + fmt.Sprintf(" %.0f%%]", ratio*100)
}

func (m Model) renderServerInfo() string {
	if m.info == nil {
		return textMuted.Render(" connecting to server…")
	}

	var parts []string

	if pct := m.info.MemoryFullness(); pct != nil && m.info.Maxmemory > 0 {
		var style lipgloss.Style
		switch {
		case *pct >= 0.90:
			style = dangerBold
		case *pct >= 0.75:
			style = warningBold
		default:
			style = successBold
		}
		parts = append(parts,
			textMuted.Render("mem: ")+style.Render(fmt.Sprintf("%5.1f%%", *pct*100))+" "+textDim.Render("("+humanize.Bytes(uint64(m.info.UsedMemory))+"/"+humanize.Bytes(uint64(m.info.Maxmemory))+")"),
		)
	} else if m.info.UsedMemory > 0 {
		parts = append(parts, textMuted.Render("mem: ")+accent.Render(humanize.Bytes(uint64(m.info.UsedMemory))))
	}

	if m.info.DBKeys > 0 {
		parts = append(parts, textMuted.Render("keys: ")+primary.Render(humanize.Comma(m.info.DBKeys)))
	}
	if m.info.EvictedKeys > 0 {
		parts = append(parts, textMuted.Render("evicted: ")+warning.Render(fmt.Sprintf("%d", m.info.EvictedKeys)))
	}
	if m.info.MemFragmentationRatio >= 1.5 {
		parts = append(parts, textMuted.Render("frag: ")+warning.Render(fmt.Sprintf("%.2f", m.info.MemFragmentationRatio)))
	}
	if m.info.RedisVersion != "" {
		parts = append(parts, textMuted.Render("ver: ")+textNormal.Render(m.info.RedisVersion))
	}

	return textDim.Render(" ") + strings.Join(parts, textDim.Render(" · "))
}

// --- Tabs ---

func (m Model) renderTabs() string {
	tabs := availableViews(m.advanced)
	var parts []string
	for _, v := range tabs {
		if v == m.view {
			parts = append(parts, lipgloss.NewStyle().
				Foreground(colSelFg).
				Background(colPrimary).
				Bold(true).
				Padding(0, 2).
				Render(v.Title()))
		} else {
			parts = append(parts, lipgloss.NewStyle().
				Foreground(colTextMuted).
				Background(compat.AdaptiveColor{Light: lipgloss.Color("#E5E7EB"), Dark: lipgloss.Color("#1F2937")}).
				Padding(0, 2).
				Render(v.Title()))
		}
	}
	return lipgloss.NewStyle().Padding(0, 1).Render(lipgloss.JoinHorizontal(lipgloss.Top, parts...))
}

// --- Body ---

func (m Model) renderBody(h int) string {
	switch m.view {
	case ViewBrowse:
		return m.renderBrowse(h)
	case ViewBiggest:
		return m.renderBiggest(h)
	case ViewPersistent:
		return m.renderPersistent(h)
	}
	return ""
}

// ==========================================================================
// Browse view — namespaces (left) + types (right)
// ==========================================================================

func (m Model) renderBrowse(h int) string {
	nsW := m.width * 38 / 100
	if nsW < 32 {
		nsW = 32
	}
	typeW := m.width - nsW - 1 // 1-space gap
	return lipgloss.JoinHorizontal(lipgloss.Top,
		m.renderNamespacePane(nsW, h),
		" ", // gap
		m.renderTypePane(typeW, h),
	)
}

func (m Model) renderNamespacePane(w, h int) string {
	rows := m.filteredNamespaces()
	if m.nsCursor >= len(rows) && len(rows) > 0 {
		m.nsCursor = 0
	}

	focus := m.pane == PaneNamespaces
	showMem := m.advanced

	// Header
	var hdr string
	if showMem {
		hdr = fmt.Sprintf("  %-14s %8s %9s", "Namespace", "Keys", "Memory")
	} else {
		hdr = fmt.Sprintf("  %-14s %8s", "Namespace", "Keys")
	}

	visibleRows := h - 4
	if visibleRows < 1 {
		visibleRows = 1
	}
	start, end := scrollWindow(m.nsCursor, len(rows), visibleRows)

	var lines []string
	lines = append(lines, textBold.Render(hdr), textMuted.Render(strings.Repeat("─", w-4)))

	for i := start; i < end; i++ {
		r := rows[i]
		sel := focus && i == m.nsCursor
		marker := "  "
		nameStyle := textNormal
		countStyle := warning
		if sel {
			marker = primary.Render("▸ ")
			nameStyle = selected
			countStyle = selected
		}

		line := marker + nameStyle.Render(padRight(trunc(internal.DisplayNamespace(r.Name), 14), 14))
		line += " " + countStyle.Render(fmt.Sprintf("%8d", r.Keys))
		if showMem {
			ms := textNormal
			if sel {
				ms = selected
			}
			line += " " + ms.Render(fmt.Sprintf("%9s", humanize.Bytes(uint64(r.Bytes))))
		}
		lines = append(lines, line)
	}

	if len(rows) == 0 {
		lines = append(lines, textMuted.Render("  No namespaces yet…"))
	}

	title := fmt.Sprintf("Namespaces · %d", len(rows))
	return box(title, focus, w, h, strings.Join(lines, "\n"))
}

func (m Model) renderTypePane(w, h int) string {
	ns := ""
	if rows := m.filteredNamespaces(); m.nsCursor < len(rows) {
		ns = rows[m.nsCursor].Name
	}
	rows := m.filteredTypeRows()
	if m.typeCursor >= len(rows) && len(rows) > 0 {
		m.typeCursor = 0
	}

	focus := m.pane == PaneTypes
	showMem := m.advanced

	// Header
	var hdr string
	if showMem {
		hdr = fmt.Sprintf("  %-28s %7s %5s %9s %9s %5s", "Type", "Keys", "%", "Total", "Avg", "P/E")
	} else {
		hdr = fmt.Sprintf("  %-28s %7s %5s", "Type", "Keys", "%")
	}

	visibleRows := h - 4
	if visibleRows < 1 {
		visibleRows = 1
	}
	start, end := scrollWindow(m.typeCursor, len(rows), visibleRows)

	var lines []string
	lines = append(lines, textBold.Render(hdr), textMuted.Render(strings.Repeat("─", w-4)))

	for i := start; i < end; i++ {
		r := rows[i]
		sel := focus && i == m.typeCursor
		marker := "  "
		if sel {
			marker = primary.Render("▸ ")
		}

		isTag := internal.IsTagType(r.Name)
		displayName := internal.DisplayType(r.Name)
		if isTag {
			displayName = strings.TrimSuffix(displayName, "  [tag]")
		}

		dataStyle := textNormal
		countStyle := warning
		if sel {
			dataStyle = selected
			countStyle = selected
		}

		maxNameW := 28
		var nameField string
		if isTag {
			badge := tagBadge
			if sel {
				badge = tagBadgeSel
			}
			nameField = badge.Render("TAG") + " " + dataStyle.Render(padRight(trunc(displayName, maxNameW-4), maxNameW-4))
		} else {
			nameField = dataStyle.Render(padRight(trunc(displayName, maxNameW), maxNameW))
		}

		line := marker + nameField
		line += " " + countStyle.Render(fmt.Sprintf("%7d", r.Keys))
		line += " " + countStyle.Render(pctFmt(r.Keys, m.stats.NamespaceCount(ns)))
		if showMem {
			line += " " + dataStyle.Render(fmt.Sprintf("%9s", humanize.Bytes(uint64(r.TotalBytes))))
			line += " " + dataStyle.Render(fmt.Sprintf("%9s", humanize.Bytes(uint64(r.AvgBytes))))
			line += " " + dataStyle.Render(fmt.Sprintf("%d/%d", r.Persistent, r.Expiring))
		}
		lines = append(lines, line)
	}

	if len(rows) == 0 && len(ns) > 0 {
		lines = append(lines, textMuted.Render("  No types in this namespace"))
	} else if len(ns) == 0 {
		lines = append(lines, textMuted.Render("  Select a namespace"))
	}

	title := fmt.Sprintf("%s · %d types", ns, len(rows))
	if ns == "" {
		title = "Types"
	}
	return box(title, focus, w, h, strings.Join(lines, "\n"))
}

// ==========================================================================
// Biggest keys view
// ==========================================================================

func (m Model) renderBiggest(h int) string {
	big := m.stats.Biggest()
	if m.bigCursor >= len(big) && len(big) > 0 {
		m.bigCursor = 0
	}

	visibleRows := h - 4
	if visibleRows < 1 {
		visibleRows = 1
	}
	start, end := scrollWindow(m.bigCursor, len(big), visibleRows)

	hdr := fmt.Sprintf("  %-4s %10s %-8s %-11s %s", "#", "Size", "Type", "TTL", "Key")
	var lines []string
	lines = append(lines, textBold.Render(hdr), textMuted.Render(strings.Repeat("─", m.width-4)))

	for i := start; i < end; i++ {
		b := big[i]
		sel := i == m.bigCursor
		marker := "  "
		if sel {
			marker = primary.Render("▸ ")
		}

		dt := "-"
		if b.DataType != nil {
			dt = *b.DataType
		}
		tl := ttlLabel(b.TTL)
		key := internal.DisplayKey(b.Key)

		ds := textNormal
		ss := success
		if sel {
			ds = selected
			ss = selected
		}

		line := marker +
			ds.Render(fmt.Sprintf("%-4d", i+1)) + " " +
			ss.Render(fmt.Sprintf("%10s", humanize.Bytes(uint64(b.Bytes)))) + " " +
			ds.Render(fmt.Sprintf("%-8s", dt)) + " " +
			ds.Render(fmt.Sprintf("%-11s", tl)) + " " +
			ds.Render(trunc(key, m.width-44))
		lines = append(lines, line)
	}

	if len(big) == 0 {
		msg := "  No keys measured"
		if !m.advanced {
			msg = "  Basic mode — relaunch with Advanced mode to track biggest keys"
		}
		lines = append(lines, textMuted.Render(msg))
	}

	title := fmt.Sprintf("Biggest keys · %d", len(big))
	if len(big) == 0 {
		title = "Biggest keys"
	}
	return box(title, true, m.width, h, strings.Join(lines, "\n"))
}

// ==========================================================================
// Persistent keys view
// ==========================================================================

func (m Model) renderPersistent(h int) string {
	rows := internal.PersistentRows(m.stats)
	if m.persistCursor >= len(rows) && len(rows) > 0 {
		m.persistCursor = 0
	}

	var persistKeys, persistBytes int64
	for _, r := range rows {
		persistKeys += r.Keys
		persistBytes += r.Bytes
	}
	var measuredBytes int64
	for _, types := range m.stats.Namespaces {
		for _, s := range types {
			measuredBytes += s.TotalBytes
		}
	}
	pctMem := 0.0
	if measuredBytes > 0 {
		pctMem = float64(persistBytes) / float64(measuredBytes) * 100.0
	}

	// Summary banner
	banner := warningBold.Render(fmt.Sprintf(" %d persistent keys", persistKeys)) +
		textDim.Render(" holding ") +
		warningBold.Render(humanize.Bytes(uint64(persistBytes))) +
		textMuted.Render(fmt.Sprintf(" (%.1f%% of measured memory — won't be freed by expiry)", pctMem))
	bannerH := 1

	visibleRows := h - bannerH - 4
	if visibleRows < 1 {
		visibleRows = 1
	}
	start, end := scrollWindow(m.persistCursor, len(rows), visibleRows)

	// Table
	hdr := fmt.Sprintf("  %-14s %-32s %8s %10s", "Namespace", "Type", "Keys", "Memory")
	var lines []string
	lines = append(lines, textBold.Render(hdr), textMuted.Render(strings.Repeat("─", m.width-4)))

	for i := start; i < end; i++ {
		r := rows[i]
		sel := i == m.persistCursor
		marker := "  "
		if sel {
			marker = primary.Render("▸ ")
		}

		isTag := internal.IsTagType(r.KeyType)
		displayName := internal.DisplayType(r.KeyType)
		if isTag {
			displayName = strings.TrimSuffix(displayName, "  [tag]")
		}

		ds := textNormal
		if sel {
			ds = selected
		}

		var typeField string
		if isTag {
			badge := tagBadge
			if sel {
				badge = tagBadgeSel
			}
			typeField = badge.Render("TAG") + " " + ds.Render(padRight(trunc(displayName, 28), 28))
		} else {
			typeField = ds.Render(padRight(trunc(displayName, 32), 32))
		}

		line := marker +
			ds.Render(padRight(trunc(internal.DisplayNamespace(r.Namespace), 14), 14)) + " " +
			typeField + " " +
			ds.Render(fmt.Sprintf("%8d", r.Keys)) + " " +
			ds.Render(fmt.Sprintf("%10s", humanize.Bytes(uint64(r.Bytes))))
		lines = append(lines, line)
	}

	if len(rows) == 0 {
		lines = append(lines, textMuted.Render("  Every key has a TTL — nothing persistent"))
	}

	title := fmt.Sprintf("Persistent · %d", len(rows))
	tableH := h - bannerH
	table := box(title, true, m.width, tableH, strings.Join(lines, "\n"))

	return lipgloss.JoinVertical(lipgloss.Top, banner, table)
}

// ==========================================================================
// Footer
// ==========================================================================

func renderShortcut(key, desc string) string {
	kStyle := lipgloss.NewStyle().
		Foreground(colSelFg).
		Background(colPrimary).
		Bold(true).
		Padding(0, 1)
	dStyle := lipgloss.NewStyle().
		Foreground(colTextDim).
		Padding(0, 1)
	return kStyle.Render(key) + dStyle.Render(desc)
}

func (m Model) renderFooter() string {
	if m.err != "" {
		return dangerBold.Padding(0, 1).Width(m.width).Render("✗ " + trunc(m.err, m.width-6))
	}

	if m.editingFilter {
		return lipgloss.NewStyle().
			Width(m.width).
			Padding(0, 1).
			Background(compat.AdaptiveColor{Light: lipgloss.Color("#E5E7EB"), Dark: lipgloss.Color("#1F2937")}).
			Render(
				primaryBold.Render(" 🔍 Filter: ") +
					textNormal.Bold(true).Render(m.filter) +
					primary.Render("█") +
					textMuted.Padding(0, 2).Render("(enter/esc to apply)"),
			)
	}

	filterTag := ""
	if m.filter != "" {
		filterTag = "  " + textMuted.Render("filter:") + " " + textBold.Render(fmt.Sprintf("%q", m.filter))
	}

	var shortcuts []string
	shortcuts = append(shortcuts, renderShortcut("q", "Quit"))
	shortcuts = append(shortcuts, renderShortcut("/", "Filter"))

	switch m.view {
	case ViewBrowse:
		shortcuts = append(shortcuts, renderShortcut("s", "Sort ["+m.sort.Label()+"]"))
		if m.pane == PaneTypes && m.client != nil {
			shortcuts = append(shortcuts, renderShortcut("enter", "Keys"))
			shortcuts = append(shortcuts, renderShortcut("esc", "Back"))
		} else {
			shortcuts = append(shortcuts, renderShortcut("enter", "Types"))
		}
		shortcuts = append(shortcuts, renderShortcut("tab", "Next View"))
	case ViewBiggest:
		shortcuts = append(shortcuts, renderShortcut("s", "Sort ["+m.sort.Label()+"]"))
		shortcuts = append(shortcuts, renderShortcut("tab", "Next View"))
	case ViewPersistent:
		shortcuts = append(shortcuts, renderShortcut("tab", "Next View"))
	}

	shortcutsBar := lipgloss.JoinHorizontal(lipgloss.Top, shortcuts...)

	footerBar := lipgloss.NewStyle().
		Background(compat.AdaptiveColor{Light: lipgloss.Color("#E5E7EB"), Dark: lipgloss.Color("#1F2937")}).
		Width(m.width).
		Render(lipgloss.JoinHorizontal(lipgloss.Top, shortcutsBar, filterTag))

	return footerBar
}

// ==========================================================================
// Drill-down: key list
// ==========================================================================

func (m Model) renderKeyList() string {
	headerH := 1
	footerH := 1
	bodyH := m.height - headerH - footerH
	if bodyH < 3 {
		bodyH = 3
	}

	header := m.renderDrillHeader("Keys")

	var shortcuts []string
	shortcuts = append(shortcuts, renderShortcut("q", "Quit"))
	shortcuts = append(shortcuts, renderShortcut("enter", "View Value"))
	shortcuts = append(shortcuts, renderShortcut("esc", "Back"))
	footer := lipgloss.NewStyle().
		Background(compat.AdaptiveColor{Light: lipgloss.Color("#E5E7EB"), Dark: lipgloss.Color("#1F2937")}).
		Width(m.width).
		Render(lipgloss.JoinHorizontal(lipgloss.Top, shortcuts...))

	var content string
	switch {
	case m.loading:
		content = primary.Render("  " + spinnerGlyph() + " Loading keys…")
	case m.keyList == nil:
		content = textMuted.Render("  No keys.")
	case m.keyList.Error != "":
		content = danger.Render("  Error: " + m.keyList.Error)
	default:
		visibleRows := bodyH - 2
		if visibleRows < 1 {
			visibleRows = 1
		}
		start, end := scrollWindow(m.keyListCursor, len(m.keyList.Keys), visibleRows)

		var lines []string
		for i := start; i < end; i++ {
			k := m.keyList.Keys[i]
			sel := i == m.keyListCursor
			marker := "  "
			s := textNormal
			if sel {
				marker = primary.Render("▸ ")
				s = selected
			}
			lines = append(lines, marker+s.Render(trunc(k, m.width-6)))
		}
		content = strings.Join(lines, "\n")
	}

	capped := ""
	if m.keyList != nil && m.keyList.Capped {
		capped = " (capped)"
	}
	title := "Keys"
	if m.keyList != nil {
		title = fmt.Sprintf("%s · %s — %d keys%s",
			m.keyList.Namespace, internal.DisplayType(m.keyList.KeyType), len(m.keyList.Keys), capped)
	}

	return lipgloss.JoinVertical(lipgloss.Top,
		header,
		box(title, true, m.width, bodyH, content),
		footer,
	)
}

// ==========================================================================
// Drill-down: value
// ==========================================================================

func (m Model) renderValue() string {
	headerH := 1
	footerH := 1
	metaH := 6
	bodyH := m.height - headerH - footerH - metaH
	if bodyH < 3 {
		bodyH = 3
	}

	header := m.renderDrillHeader("Value")

	var shortcuts []string
	shortcuts = append(shortcuts, renderShortcut("q", "Quit"))
	shortcuts = append(shortcuts, renderShortcut("x", "Toggle Hex/Text"))
	shortcuts = append(shortcuts, renderShortcut("esc", "Back"))
	shortcuts = append(shortcuts, renderShortcut("↑↓", "Scroll"))
	footer := lipgloss.NewStyle().
		Background(compat.AdaptiveColor{Light: lipgloss.Color("#E5E7EB"), Dark: lipgloss.Color("#1F2937")}).
		Width(m.width).
		Render(lipgloss.JoinHorizontal(lipgloss.Top, shortcuts...))

	// Metadata
	var metaContent string
	switch {
	case m.loading:
		metaContent = primary.Render("  " + spinnerGlyph() + " Loading value…")
	case m.keyValue == nil:
		metaContent = textMuted.Render("  No value.")
	case m.keyValue.Error != "":
		metaContent = danger.Render("  Error: " + m.keyValue.Error)
	default:
		kv := m.keyValue
		var lines []string
		lines = append(lines, textBold.Render("  "+trunc(kv.Key, m.width-6)))

		line2 := "  " + primary.Render("type ") + textNormal.Render(kv.RedisType)
		if kv.Encoding != internal.EncodingPlain {
			lbl := fmt.Sprintf("decompressed from %s", kv.Encoding.Label())
			if kv.StreamOffset != nil && *kv.StreamOffset > 0 {
				lbl = fmt.Sprintf("decompressed %s (embedded @ %d)", kv.Encoding.Label(), *kv.StreamOffset)
			}
			line2 += textDim.Render(" · ") + warning.Render(lbl)
		}
		lines = append(lines, line2)

		var infoParts []string
		if kv.SizeBytes != nil {
			infoParts = append(infoParts, accent.Render(fmt.Sprintf("size %s", humanize.Bytes(uint64(*kv.SizeBytes)))))
		}
		if kv.Elements != nil {
			infoParts = append(infoParts, textNormal.Render(fmt.Sprintf("elements %d", *kv.Elements)))
		}
		infoParts = append(infoParts, textNormal.Render("ttl "+internal.TTLWithExpiry(kv.TTL)))
		lines = append(lines, "  "+strings.Join(infoParts, textDim.Render(" · ")))

		metaContent = strings.Join(lines, "\n")
	}

	metaBox := box("Key", false, m.width, metaH, metaContent)

	// Body
	var bodyContent, bodyTitle string
	if m.keyValue != nil && m.keyValue.Error == "" && !m.loading {
		kv := m.keyValue
		showHex := m.valueHex && kv.Hex != ""

		if showHex {
			bodyContent = kv.Hex
			sz := int64(0)
			if kv.SizeBytes != nil {
				sz = *kv.SizeBytes
			}
			bodyTitle = fmt.Sprintf("Value · hex · %d bytes (x: text)", sz)
		} else {
			toggle := ""
			if kv.Hex != "" {
				toggle = " (x: hex)"
			}
			tag := ""
			if kv.IsBinary {
				tag = " · binary"
			}
			bodyContent = kv.Body
			bodyTitle = fmt.Sprintf("Value%s%s%s", tag, "", toggle)
		}

		// Apply scroll
		lines := strings.Split(bodyContent, "\n")
		if m.valueScroll < len(lines) {
			bodyContent = strings.Join(lines[m.valueScroll:], "\n")
		} else {
			bodyContent = ""
		}
	} else if !m.loading && m.keyValue == nil {
		bodyContent = ""
		bodyTitle = "Value"
	} else if m.loading {
		bodyContent = ""
		bodyTitle = "Value"
	} else {
		bodyContent = ""
		bodyTitle = "Value"
	}

	bodyBox := box(bodyTitle, true, m.width, bodyH, bodyContent)

	return lipgloss.JoinVertical(lipgloss.Top, header, metaBox, bodyBox, footer)
}

// --- Drill header ---

func (m Model) renderDrillHeader(what string) string {
	title := primaryBold.Render(" SHOPWARE REDIS INSIGHTS ")
	arrow := textMuted.Render(" ❯ ")
	section := textNormal.Bold(true).Render(strings.ToUpper(what))

	rightSide := ""
	if m.info != nil && m.info.RedisVersion != "" {
		rightSide = textMuted.Render("v" + m.info.RedisVersion + " ")
	}

	leftSide := title + arrow + section
	paddingCount := m.width - lipgloss.Width(leftSide) - lipgloss.Width(rightSide)
	if paddingCount < 0 {
		paddingCount = 0
	}

	headerBar := leftSide + strings.Repeat(" ", paddingCount) + rightSide

	return lipgloss.NewStyle().
		Background(compat.AdaptiveColor{Light: lipgloss.Color("#E5E7EB"), Dark: lipgloss.Color("#1F2937")}).
		Padding(0, 1).
		Width(m.width).
		Render(headerBar)
}

func scrollWindow(cursor, maxItems, height int) (int, int) {
	if maxItems <= height {
		return 0, maxItems
	}
	start := cursor - height/2
	if start < 0 {
		start = 0
	}
	end := start + height
	if end > maxItems {
		end = maxItems
		start = end - height
	}
	if start < 0 {
		start = 0
	}
	return start, end
}
