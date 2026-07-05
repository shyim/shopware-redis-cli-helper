package internal

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"

	"github.com/dustin/go-humanize"
)

// ReportOptions configures what a report includes.
type ReportOptions struct {
	ShowMemory bool
	ShowTypes  bool
	ShowTTL    bool
	Top        int // only show top N types per namespace (0 = all)
	Biggest    int // how many biggest keys to list (0 = omit)
}

func pct(part, whole int64) string {
	if whole == 0 {
		return "0.0%"
	}
	return fmt.Sprintf("%.1f%%", float64(part)/float64(whole)*100.0)
}

func ttlLabel(ttl *int64) string {
	if ttl == nil {
		return "-"
	}
	switch *ttl {
	case -1:
		return "persistent"
	case -2:
		return "-"
	}
	if *ttl >= 0 {
		return fmt.Sprintf("%ds", *ttl)
	}
	return "-"
}

type nsRow struct {
	name  string
	count int64
	bytes int64
}

func namespaceRows(stats *Stats) []nsRow {
	var rows []nsRow
	for ns := range stats.Namespaces {
		rows = append(rows, nsRow{
			name:  ns,
			count: stats.NamespaceCount(ns),
			bytes: stats.NamespaceBytes(ns),
		})
	}
	sort.Slice(rows, func(i, j int) bool {
		if rows[i].count == rows[j].count {
			return rows[i].name < rows[j].name
		}
		return rows[i].count > rows[j].count
	})
	return rows
}

// RenderMarkdown renders stats as a Markdown document.
func RenderMarkdown(stats *Stats, opts *ReportOptions) string {
	var out strings.Builder
	nsRows := namespaceRows(stats)

	fmt.Fprintf(&out, "# Redis Insights Report\n\n")
	fmt.Fprintf(&out, "- **Total keys scanned:** %d\n", stats.TotalKeys)
	fmt.Fprintf(&out, "- **Namespaces:** %d\n", len(nsRows))
	if opts.ShowMemory {
		var totalBytes int64
		for _, r := range nsRows {
			totalBytes += r.bytes
		}
		fmt.Fprintf(&out, "- **Measured memory:** %s\n", humanize.Bytes(uint64(totalBytes)))
	}
	out.WriteString("\n")

	// Namespace summary
	fmt.Fprintf(&out, "## Namespace summary\n\n")
	if opts.ShowMemory {
		fmt.Fprintf(&out, "| Namespace | Keys | %% | Memory |\n")
		fmt.Fprintf(&out, "|---|---:|---:|---:|\n")
		for _, r := range nsRows {
			fmt.Fprintf(&out, "| %s | %d | %s | %s |\n",
				mdEscape(r.name), r.count, pct(r.count, stats.TotalKeys), humanize.Bytes(uint64(r.bytes)))
		}
	} else {
		fmt.Fprintf(&out, "| Namespace | Keys | %% |\n")
		fmt.Fprintf(&out, "|---|---:|---:|\n")
		for _, r := range nsRows {
			fmt.Fprintf(&out, "| %s | %d | %s |\n",
				mdEscape(r.name), r.count, pct(r.count, stats.TotalKeys))
		}
	}
	out.WriteString("\n")

	// Per-namespace type breakdown
	for _, nsr := range nsRows {
		types := stats.Namespaces[nsr.name]
		var typeRows []typeRowData
		for name, stat := range types {
			typeRows = append(typeRows, typeRowData{name: name, stat: stat})
		}
		sort.Slice(typeRows, func(i, j int) bool {
			if typeRows[i].stat.Count == typeRows[j].stat.Count {
				return typeRows[i].name < typeRows[j].name
			}
			return typeRows[i].stat.Count > typeRows[j].stat.Count
		})

		total := stats.NamespaceCount(nsr.name)
		shown := len(typeRows)
		if opts.Top > 0 && opts.Top < shown {
			shown = opts.Top
		}

		fmt.Fprintf(&out, "## `%s` — %d keys, %d types\n\n", mdEscape(nsr.name), total, len(typeRows))

		head := "| Type | Keys | %"
		sep := "|---|---:|---:"
		if opts.ShowMemory {
			head += " | Total mem | Avg mem"
			sep += "|---:|---:"
		}
		if opts.ShowTypes {
			head += " | Data types"
			sep += "|---"
		}
		if opts.ShowTTL {
			head += " | Persist/Expire"
			sep += "|---"
		}
		head += " |"
		sep += "|"
		fmt.Fprintf(&out, "%s\n%s\n", head, sep)

		for i := 0; i < shown; i++ {
			tr := typeRows[i]
			fmt.Fprintf(&out, "| `%s` | %d | %s",
				mdEscape(tr.name), tr.stat.Count, pct(tr.stat.Count, total))
			if opts.ShowMemory {
				fmt.Fprintf(&out, " | %s | %s",
					humanize.Bytes(uint64(tr.stat.TotalBytes)),
					humanize.Bytes(uint64(tr.stat.AvgBytes())))
			}
			if opts.ShowTypes {
				fmt.Fprintf(&out, " | %s", mdEscape(dataTypesStr(tr.stat)))
			}
			if opts.ShowTTL {
				fmt.Fprintf(&out, " | %d/%d", tr.stat.Persistent, tr.stat.Expiring)
			}
			fmt.Fprintf(&out, " |\n")
		}

		if shown < len(typeRows) {
			var hidden int64
			for i := shown; i < len(typeRows); i++ {
				hidden += typeRows[i].stat.Count
			}
			fmt.Fprintf(&out, "\n_%d more types hidden (%d keys)._\n", len(typeRows)-shown, hidden)
		}
		out.WriteString("\n")
	}

	// Biggest keys
	if opts.Biggest > 0 {
		big := stats.Biggest()
		if len(big) > 0 {
			fmt.Fprintf(&out, "## Biggest %d keys\n\n", len(big))
			fmt.Fprintf(&out, "| # | Size | Type | TTL | Key |\n")
			fmt.Fprintf(&out, "|---:|---:|---|---|---|\n")
			for i, b := range big {
				dt := "-"
				if b.DataType != nil {
					dt = *b.DataType
				}
				fmt.Fprintf(&out, "| %d | %s | %s | %s | `%s` |\n",
					i+1, humanize.Bytes(uint64(b.Bytes)), dt, ttlLabel(b.TTL), mdEscape(b.Key))
			}
			out.WriteString("\n")
		}
	}

	return out.String()
}

// RenderTable renders stats as a terminal table (using plain text formatting).
func RenderTable(stats *Stats, opts *ReportOptions) string {
	var out strings.Builder
	nsRows := namespaceRows(stats)

	fmt.Fprintf(&out, "\n=== Namespace summary ===\n")

	// Header
	header := fmt.Sprintf("%-20s %10s %7s", "Namespace", "Keys", "%")
	if opts.ShowMemory {
		header += fmt.Sprintf(" %10s", "Memory")
	}
	fmt.Fprintf(&out, "%s\n", header)
	fmt.Fprintf(&out, "%s\n", strings.Repeat("-", len(header)))

	for _, r := range nsRows {
		line := fmt.Sprintf("%-20s %10d %7s", trunc(r.name, 20), r.count, pct(r.count, stats.TotalKeys))
		if opts.ShowMemory {
			line += fmt.Sprintf(" %10s", humanize.Bytes(uint64(r.bytes)))
		}
		fmt.Fprintf(&out, "%s\n", line)
	}
	fmt.Fprintf(&out, "Total keys: %d\n", stats.TotalKeys)

	for _, nsr := range nsRows {
		types := stats.Namespaces[nsr.name]
		var typeRows []typeRowData
		for name, stat := range types {
			typeRows = append(typeRows, typeRowData{name: name, stat: stat})
		}
		sort.Slice(typeRows, func(i, j int) bool {
			if typeRows[i].stat.Count == typeRows[j].stat.Count {
				return typeRows[i].name < typeRows[j].name
			}
			return typeRows[i].stat.Count > typeRows[j].stat.Count
		})

		total := stats.NamespaceCount(nsr.name)
		shown := len(typeRows)
		if opts.Top > 0 && opts.Top < shown {
			shown = opts.Top
		}

		fmt.Fprintf(&out, "\n--- %s (%d keys, %d types) ---\n", nsr.name, total, len(typeRows))

		for i := 0; i < shown; i++ {
			tr := typeRows[i]
			line := fmt.Sprintf("  %-40s %8d %7s", trunc(tr.name, 40), tr.stat.Count, pct(tr.stat.Count, total))
			if opts.ShowMemory {
				line += fmt.Sprintf(" %10s %10s",
					humanize.Bytes(uint64(tr.stat.TotalBytes)),
					humanize.Bytes(uint64(tr.stat.AvgBytes())))
			}
			if opts.ShowTypes {
				line += fmt.Sprintf(" %s", trunc(dataTypesStr(tr.stat), 20))
			}
			if opts.ShowTTL {
				line += fmt.Sprintf(" %d/%d", tr.stat.Persistent, tr.stat.Expiring)
			}
			fmt.Fprintf(&out, "%s\n", line)
		}

		if shown < len(typeRows) {
			var hidden int64
			for i := shown; i < len(typeRows); i++ {
				hidden += typeRows[i].stat.Count
			}
			fmt.Fprintf(&out, "  … %d more types hidden (%d keys).\n", len(typeRows)-shown, hidden)
		}
	}

	// Biggest keys
	if opts.Biggest > 0 {
		big := stats.Biggest()
		if len(big) > 0 {
			fmt.Fprintf(&out, "\n=== Biggest %d keys ===\n", len(big))
			fmt.Fprintf(&out, "%-5s %10s %-8s %-12s %s\n", "#", "Size", "Type", "TTL", "Key")
			fmt.Fprintf(&out, "%s\n", strings.Repeat("-", 70))
			for i, b := range big {
				dt := "-"
				if b.DataType != nil {
					dt = *b.DataType
				}
				fmt.Fprintf(&out, "%-5d %10s %-8s %-12s %s\n",
					i+1, humanize.Bytes(uint64(b.Bytes)), dt, ttlLabel(b.TTL), trunc(b.Key, 40))
			}
		}
	}

	return out.String()
}

// RenderJSON renders stats as JSON.
func RenderJSON(stats *Stats) string {
	type jsonReport struct {
		*Stats
		Biggest []BigKey `json:"biggest"`
	}
	b, _ := json.MarshalIndent(jsonReport{Stats: stats, Biggest: stats.Biggest()}, "", "  ")
	return string(b)
}

type typeRowData struct {
	name string
	stat *TypeStat
}

func dataTypesStr(stat *TypeStat) string {
	type dtPair struct {
		name  string
		count int64
	}
	var pairs []dtPair
	for k, v := range stat.DataTypes {
		pairs = append(pairs, dtPair{k, v})
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

func mdEscape(s string) string {
	s = strings.ReplaceAll(s, "|", "\\|")
	s = strings.ReplaceAll(s, "\n", " ")
	s = strings.ReplaceAll(s, "\r", " ")
	return s
}

func trunc(s string, max int) string {
	runes := []rune(s)
	if len(runes) <= max {
		return s
	}
	return string(runes[:max-1]) + "…"
}

// PersistRow represents a type with persistent (no-TTL) keys.
type PersistRow struct {
	Namespace string
	KeyType   string
	Keys      int64
	Bytes     int64
}

// PersistentRows returns all (namespace, type) buckets with persistent keys,
// sorted by memory (descending).
func PersistentRows(stats *Stats) []PersistRow {
	var rows []PersistRow
	for ns, types := range stats.Namespaces {
		for t, s := range types {
			if s.Persistent > 0 {
				rows = append(rows, PersistRow{
					Namespace: ns,
					KeyType:   t,
					Keys:      s.Persistent,
					Bytes:     s.PersistentBytes,
				})
			}
		}
	}
	sort.Slice(rows, func(i, j int) bool {
		if rows[i].Bytes != rows[j].Bytes {
			return rows[i].Bytes > rows[j].Bytes
		}
		if rows[i].Keys != rows[j].Keys {
			return rows[i].Keys > rows[j].Keys
		}
		if rows[i].Namespace != rows[j].Namespace {
			return rows[i].Namespace < rows[j].Namespace
		}
		return rows[i].KeyType < rows[j].KeyType
	})
	return rows
}
