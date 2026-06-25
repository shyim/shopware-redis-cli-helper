//! Rendering of aggregated stats as terminal tables or a Markdown document.

use crate::stats::{BigKey, Stats};
use comfy_table::{Cell, CellAlignment, Table};
use humansize::{format_size, DECIMAL};
use std::fmt::Write as _;

pub struct ReportOptions {
    pub show_memory: bool,
    pub show_types: bool,
    pub show_ttl: bool,
    /// Only print the top N types per namespace (0 = all).
    pub top: usize,
    /// How many biggest keys to list (0 = section omitted).
    pub biggest: usize,
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "0.0%".to_string()
    } else {
        format!("{:.1}%", part as f64 / whole as f64 * 100.0)
    }
}

fn ttl_label(ttl: Option<i64>) -> String {
    match ttl {
        Some(-1) => "persistent".to_string(),
        Some(t) if t >= 0 => format!("{t}s"),
        _ => "-".to_string(),
    }
}

/// Sorted (namespace, key-count, total-bytes) rows, biggest first.
fn namespace_rows(stats: &Stats) -> Vec<(&String, u64, u64)> {
    let mut rows: Vec<(&String, u64, u64)> = stats
        .namespaces
        .keys()
        .map(|ns| (ns, stats.namespace_count(ns), stats.namespace_bytes(ns)))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    rows
}

// ---------------------------------------------------------------------------
// Terminal (comfy-table) rendering
// ---------------------------------------------------------------------------

pub fn render(stats: &Stats, opts: &ReportOptions) {
    let ns_rows = namespace_rows(stats);

    println!("\n=== Namespace summary ===");
    let mut t = Table::new();
    let mut header = vec!["Namespace", "Keys", "%"];
    if opts.show_memory {
        header.push("Memory");
    }
    t.set_header(header);
    for (ns, count, bytes) in &ns_rows {
        let mut row = vec![
            Cell::new(ns),
            Cell::new(count).set_alignment(CellAlignment::Right),
            Cell::new(pct(*count, stats.total_keys)).set_alignment(CellAlignment::Right),
        ];
        if opts.show_memory {
            row.push(Cell::new(format_size(*bytes, DECIMAL)).set_alignment(CellAlignment::Right));
        }
        t.add_row(row);
    }
    println!("{t}");
    println!("Total keys: {}", stats.total_keys);

    for (ns, _, _) in &ns_rows {
        let types = &stats.namespaces[*ns];
        let mut rows: Vec<_> = types.iter().collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1.count));
        let total = stats.namespace_count(ns);
        let shown = if opts.top == 0 {
            rows.len()
        } else {
            opts.top.min(rows.len())
        };

        println!("\n--- {} ({} keys, {} types) ---", ns, total, rows.len());
        let mut t = Table::new();
        let mut header = vec!["Type", "Keys", "%"];
        if opts.show_memory {
            header.push("Total mem");
            header.push("Avg mem");
        }
        if opts.show_types {
            header.push("Data types");
        }
        if opts.show_ttl {
            header.push("Persist/Expire");
        }
        t.set_header(header);

        for (name, stat) in rows.iter().take(shown) {
            let mut row = vec![
                Cell::new(name),
                Cell::new(stat.count).set_alignment(CellAlignment::Right),
                Cell::new(pct(stat.count, total)).set_alignment(CellAlignment::Right),
            ];
            if opts.show_memory {
                row.push(
                    Cell::new(format_size(stat.total_bytes, DECIMAL))
                        .set_alignment(CellAlignment::Right),
                );
                row.push(
                    Cell::new(format_size(stat.avg_bytes(), DECIMAL))
                        .set_alignment(CellAlignment::Right),
                );
            }
            if opts.show_types {
                row.push(Cell::new(data_types_str(stat)));
            }
            if opts.show_ttl {
                row.push(Cell::new(format!("{}/{}", stat.persistent, stat.expiring)));
            }
            t.add_row(row);
        }
        println!("{t}");
        if shown < rows.len() {
            let hidden: u64 = rows.iter().skip(shown).map(|(_, s)| s.count).sum();
            println!(
                "  … {} more types hidden ({} keys). Use --top 0 to show all.",
                rows.len() - shown,
                hidden
            );
        }
    }

    render_biggest_terminal(stats, opts);
}

fn render_biggest_terminal(stats: &Stats, opts: &ReportOptions) {
    if opts.biggest == 0 {
        return;
    }
    let big = stats.biggest();
    if big.is_empty() {
        return;
    }
    println!("\n=== Biggest {} keys ===", big.len());
    let mut t = Table::new();
    t.set_header(vec!["#", "Size", "Type", "TTL", "Key"]);
    for (i, b) in big.iter().enumerate() {
        t.add_row(vec![
            Cell::new(i + 1).set_alignment(CellAlignment::Right),
            Cell::new(format_size(b.bytes, DECIMAL)).set_alignment(CellAlignment::Right),
            Cell::new(b.data_type.as_deref().unwrap_or("-")),
            Cell::new(ttl_label(b.ttl)),
            Cell::new(&b.key),
        ]);
    }
    println!("{t}");
}

fn data_types_str(stat: &crate::stats::TypeStat) -> String {
    let mut dts: Vec<_> = stat.data_types.iter().collect();
    dts.sort_by(|a, b| b.1.cmp(a.1));
    dts.iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

/// Escape the pipe and control characters so a key can't break a Markdown table.
fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// `\x01` is a control byte; wrap keys in backticks so it renders literally.
fn md_key(s: &str) -> String {
    format!("`{}`", md_escape(s))
}

pub fn render_markdown(stats: &Stats, opts: &ReportOptions) -> String {
    let mut out = String::new();
    let ns_rows = namespace_rows(stats);

    writeln!(out, "# Redis Insights Report\n").unwrap();
    writeln!(out, "- **Total keys scanned:** {}", stats.total_keys).unwrap();
    writeln!(out, "- **Namespaces:** {}", ns_rows.len()).unwrap();
    if opts.show_memory {
        let total_bytes: u64 = ns_rows.iter().map(|(_, _, b)| *b).sum();
        writeln!(
            out,
            "- **Measured memory:** {}",
            format_size(total_bytes, DECIMAL)
        )
        .unwrap();
    }
    out.push('\n');

    // Namespace summary
    writeln!(out, "## Namespace summary\n").unwrap();
    if opts.show_memory {
        writeln!(out, "| Namespace | Keys | % | Memory |").unwrap();
        writeln!(out, "|---|--:|--:|--:|").unwrap();
        for (ns, count, bytes) in &ns_rows {
            writeln!(
                out,
                "| {} | {} | {} | {} |",
                md_escape(ns),
                count,
                pct(*count, stats.total_keys),
                format_size(*bytes, DECIMAL)
            )
            .unwrap();
        }
    } else {
        writeln!(out, "| Namespace | Keys | % |").unwrap();
        writeln!(out, "|---|--:|--:|").unwrap();
        for (ns, count, _) in &ns_rows {
            writeln!(
                out,
                "| {} | {} | {} |",
                md_escape(ns),
                count,
                pct(*count, stats.total_keys)
            )
            .unwrap();
        }
    }
    out.push('\n');

    // Per-namespace type breakdown
    for (ns, _, _) in &ns_rows {
        let types = &stats.namespaces[*ns];
        let mut rows: Vec<_> = types.iter().collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1.count));
        let total = stats.namespace_count(ns);
        let shown = if opts.top == 0 {
            rows.len()
        } else {
            opts.top.min(rows.len())
        };

        writeln!(
            out,
            "## `{}` — {} keys, {} types\n",
            md_escape(ns),
            total,
            rows.len()
        )
        .unwrap();

        let mut head = String::from("| Type | Keys | %");
        let mut sep = String::from("|---|--:|--:");
        if opts.show_memory {
            head.push_str(" | Total mem | Avg mem");
            sep.push_str("|--:|--:");
        }
        if opts.show_types {
            head.push_str(" | Data types");
            sep.push_str("|---");
        }
        if opts.show_ttl {
            head.push_str(" | Persist/Expire");
            sep.push_str("|---");
        }
        head.push_str(" |");
        sep.push('|');
        writeln!(out, "{head}").unwrap();
        writeln!(out, "{sep}").unwrap();

        for (name, stat) in rows.iter().take(shown) {
            write!(
                out,
                "| {} | {} | {}",
                md_key(name),
                stat.count,
                pct(stat.count, total)
            )
            .unwrap();
            if opts.show_memory {
                write!(
                    out,
                    " | {} | {}",
                    format_size(stat.total_bytes, DECIMAL),
                    format_size(stat.avg_bytes(), DECIMAL)
                )
                .unwrap();
            }
            if opts.show_types {
                write!(out, " | {}", md_escape(&data_types_str(stat))).unwrap();
            }
            if opts.show_ttl {
                write!(out, " | {}/{}", stat.persistent, stat.expiring).unwrap();
            }
            writeln!(out, " |").unwrap();
        }
        if shown < rows.len() {
            let hidden: u64 = rows.iter().skip(shown).map(|(_, s)| s.count).sum();
            writeln!(
                out,
                "\n_{} more types hidden ({} keys)._",
                rows.len() - shown,
                hidden
            )
            .unwrap();
        }
        out.push('\n');
    }

    render_biggest_markdown(&mut out, &stats.biggest(), opts);
    out
}

fn render_biggest_markdown(out: &mut String, big: &[BigKey], opts: &ReportOptions) {
    if opts.biggest == 0 || big.is_empty() {
        return;
    }
    writeln!(out, "## Biggest {} keys\n", big.len()).unwrap();
    writeln!(out, "| # | Size | Type | TTL | Key |").unwrap();
    writeln!(out, "|--:|--:|---|---|---|").unwrap();
    for (i, b) in big.iter().enumerate() {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            i + 1,
            format_size(b.bytes, DECIMAL),
            b.data_type.as_deref().unwrap_or("-"),
            ttl_label(b.ttl),
            md_key(&b.key)
        )
        .unwrap();
    }
    out.push('\n');
}
