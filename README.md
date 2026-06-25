# shopware-redis-cli-helper

A fast, standalone CLI for inspecting and maintaining a Shopware / Symfony Redis
cache. It explores the keyspace by namespace and key type, lets you drill into
individual keys (with automatic decompression), and prunes orphaned cache tags —
all over non-blocking `SCAN`, safe to run against production.

[![CI](https://github.com/shyim/shopware-redis-cli-helper/actions/workflows/ci.yml/badge.svg)](https://github.com/shyim/shopware-redis-cli-helper/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/shyim/shopware-redis-cli-helper)](https://github.com/shyim/shopware-redis-cli-helper/releases/latest)
[![Built with Rust](https://img.shields.io/badge/built_with-Rust-orange)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](#license)

## Highlights

- **Namespace & type insights** — group millions of keys into a handful of
  meaningful buckets, with key counts, memory, data type, and TTL breakdowns.
- **Interactive TUI** — a live, navigable view that fills in as the scan runs,
  with drill-down from namespace → type → key → value.
- **Value inspector** — type-aware rendering with automatic **gzip / zlib / zstd**
  decompression (including streams embedded in Symfony cache wrappers), a
  hex/text toggle, and absolute TTL expiry timestamps.
- **Orphaned-tag cleanup** — a faster, concurrent port of FroshTools'
  `frosh:redis-tag:cleanup`, dry-run by default.
- **Scriptable** — `report` (table / Markdown / JSON) and `get` (single-key
  value) commands for non-interactive use and pipelines.

## Screenshots

Browsing namespaces and their key types in the TUI:

![Browse view: namespaces on the left, type breakdown on the right](https://i.imgur.com/teRsQiB.png)

Inspecting a value — note the automatic decompression of a zlib stream embedded
in a Symfony cache wrapper, and the TTL shown with its absolute expiry:

![Value inspector showing a decompressed Shopware system-config key](https://i.imgur.com/kS6PbHe.png)

## Contents

- [Screenshots](#screenshots)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Commands](#commands)
  - [`insights` — explore the cache](#insights--explore-the-cache)
  - [`get` — read a single key](#get--read-a-single-key)
  - [`cleanup` — prune orphaned tags](#cleanup--prune-orphaned-tags)
- [Why it's production-safe](#why-its-production-safe)
- [How keys are grouped](#how-keys-are-grouped)
- [Development](#development)
- [License](#license)

## Installation

### Prebuilt binaries (recommended)

Download the archive for your platform from the
[latest release](https://github.com/shyim/shopware-redis-cli-helper/releases/latest).
Builds are published for macOS and Linux, on both Intel (`amd64`) and Apple
Silicon / ARM (`arm64`).

```bash
# Pick the matching OS/arch — e.g. macOS arm64:
VERSION=0.1.0
OS=darwin   # darwin | linux
ARCH=arm64  # arm64 | amd64

curl -fsSL -o shopware-redis-cli-helper.tar.gz \
  "https://github.com/shyim/shopware-redis-cli-helper/releases/download/v${VERSION}/shopware-redis-cli-helper_${VERSION}_${OS}_${ARCH}.tar.gz"

tar -xzf shopware-redis-cli-helper.tar.gz
sudo mv shopware-redis-cli-helper /usr/local/bin/
shopware-redis-cli-helper --version
```

Each release also ships a `checksums.txt` for verification.

### From source

Requires a Rust toolchain (edition 2021, stable).

```bash
cargo install --git https://github.com/shyim/shopware-redis-cli-helper

# or, from a checkout:
cargo build --release   # binary at ./target/release/shopware-redis-cli-helper
```

## Quick start

```bash
# Explore interactively (the TUI is the default on a terminal)
shopware-redis-cli-helper --url redis://127.0.0.1:6379/0 insights

# One-shot report to stdout
shopware-redis-cli-helper --url redis://127.0.0.1:6379/0 insights report

# Read and decode a single key
shopware-redis-cli-helper --url redis://127.0.0.1:6379/0 get "<key>"

# Preview an orphaned-tag cleanup (dry run)
shopware-redis-cli-helper --url redis://127.0.0.1:6379/0 cleanup
```

The connection target may be given with `--url` or the `REDIS_URL` environment
variable.

## Commands

```text
shopware-redis-cli-helper [--url URL] [--connect-timeout SECS] <COMMAND>

  insights [tui|report]   Scan and explore the cache
    tui                   Interactive live viewer (default on a terminal)
    report                One-shot output: --format table|markdown|json
  get <KEY>               Print one key's decoded value
  cleanup                 Remove orphaned cache tags (dry run unless --apply)
```

| Global option        | Default                  | Description                                            |
|----------------------|--------------------------|--------------------------------------------------------|
| `--url`              | `redis://127.0.0.1:6379` | Connection URL (or set `REDIS_URL`)                    |
| `--connect-timeout`  | `5`                      | Seconds to wait before failing fast on a bad host/port |

Global options work before or after the subcommand.

### `insights` — explore the cache

`insights` has two subcommands: an interactive `tui` and a one-shot `report`.
Running `insights` with no subcommand opens the TUI on a terminal, and prints a
hint when output is piped.

#### Interactive TUI

The TUI scans in the background and fills in live, so you can begin browsing
before the scan completes.

```bash
shopware-redis-cli-helper insights        # opens the TUI
shopware-redis-cli-helper insights tui     # explicit
```

**Scan depth.** On startup the TUI presents an in-app scan-depth picker:

| Mode         | Collects                                              | Cost                                   |
|--------------|------------------------------------------------------|----------------------------------------|
| **Basic**    | Key counts only                                      | One `SCAN` pass — fast on huge DBs     |
| **Advanced** | Memory, data type, TTL, and the biggest-keys ranking | Extra commands per key — slower        |

The scan starts only after you choose. Passing `--mode basic\|advanced`, or any
stat flag (`--memory`, `--full`, `--biggest N`, …), skips the picker and honors
exactly what you requested.

**Navigation.**

| Key                | Action                                                  |
|--------------------|---------------------------------------------------------|
| `↑` / `↓`, `j` / `k` | Move within the focused pane or list                  |
| `→` / `l`, `Tab`   | Focus the **Types** pane (`Tab` cycles panes)           |
| `Enter`            | Drill in: Namespaces → Types → key list → value         |
| `←` / `h`, `Esc`   | Step back out                                           |
| `x`                | In a value: toggle **hex** ⇄ **text**                   |
| `b`                | Switch **Browse** ⇄ **Biggest keys**                    |
| `s`                | Cycle sort: count → total memory → average memory       |
| `/`                | Filter namespaces / types by substring                  |
| `q` / `Ctrl-C`     | Quit                                                    |

**Value inspector.** Pressing `Enter` on a key opens a type-aware value view:

- **Strings** are decoded; **sets, lists, hashes, and zsets** are rendered as
  members, `field = value`, or `score member` (large collections are truncated).
- **Compressed values** are decompressed automatically — gzip, zlib/deflate, and
  zstd are detected by magic bytes. Symfony wraps cache items in a binary header
  and serialized tag envelope with the value compressed *inside*; the inspector
  finds that embedded stream and shows the decompressed inner value (e.g.
  *"decompressed zlib/deflate (embedded at offset 52)"*).
- **Binary values** (e.g. PHP-serialized blobs) default to an offset/hex/ASCII
  dump; press `x` to switch to a lossy text view and back.
- The header shows type, memory size, element count, and **TTL with its absolute
  expiry** — e.g. `3600s (expires 2026-06-25 15:34:36)`.

Symfony tag-index sets (stored with `\x01tags\x01` control bytes) are shown as
**🏷 `<name>` (tag set)**; filter for them by typing `tag`.

#### Reports (`insights report`)

`report` runs a single scan and renders it in the chosen format. It collects
only the statistics its flags request, so with no flags it is a fast,
counts-only scan.

```bash
B="shopware-redis-cli-helper --url redis://127.0.0.1:6379/0"

$B insights report                                   # terminal table, counts only
$B insights report --full                            # + memory, data types, TTL
$B insights report --full --biggest 50 --format markdown -o report.md
$B insights report --full --biggest 50 --format json    -o report.json

$B insights report --pattern 'WpDcVyo5fP:*'          # scope server-side (SCAN MATCH)
$B insights report --limit 100000                    # sample, then stop
```

**Scan options** (shared by `tui` and `report`):

| Flag              | Default | Description                                              |
|-------------------|---------|----------------------------------------------------------|
| `--pattern`       | (none)  | Server-side `SCAN MATCH` glob                            |
| `--count`         | `1000`  | `SCAN COUNT` hint — keys per server-side step            |
| `--limit`         | `0`     | Stop after N keys (`0` = all); use for sampling          |
| `--memory`        | off     | Collect `MEMORY USAGE` per key                           |
| `--types`         | off     | Collect Redis `TYPE` per key                             |
| `--ttl`           | off     | Collect TTL / expiry per key                             |
| `--full`          | off     | Shortcut for `--memory --types --ttl`                    |
| `--biggest N`     | `0`     | Track the N largest keys by memory (implies `--memory`)  |
| `--namespace-len` | `10`    | Fallback namespace length for keys without a `:`         |

`tui` additionally accepts `--mode basic\|advanced`. `report` additionally
accepts `--format table\|markdown\|json` (default `table`), `--output`/`-o FILE`,
and `--top N` (types shown per namespace).

> When writing a table to a file with `-o`, output falls back to Markdown — the
> file-friendly tabular format. Use `--format markdown` or `json` for files.

**Biggest keys.** `--biggest N` maintains a bounded min-heap of the N largest
keys, so memory stays constant regardless of keyspace size — the full keyspace is
never buffered. Ranking uses `MEMORY USAGE`, so `--biggest` implies `--memory`.
The leaderboard appears as its own section across table, Markdown, and JSON
output.

<details>
<summary>Example output</summary>

```text
=== Namespace summary ===
+------------+------+-------+--------+
| Namespace  | Keys | %     | Memory |
+====================================+
| WpDcVyo5fP |    6 | 85.7% |  816 B |
| OldNspace1 |    1 | 14.3% |  128 B |
+------------+------+-------+--------+

--- WpDcVyo5fP (6 keys, 5 types) ---
+----------------------+------+-------+-----------+---------+------------+----------------+
| Type                 | Keys | %     | Total mem | Avg mem | Data types | Persist/Expire |
+=========================================================================================+
| cached-product       |    2 | 33.3% |     352 B |   176 B | string:2   | 0/2            |
| \x01tags\x01product  |    1 | 16.7% |     112 B |   112 B | set:1      | 1/0            |
| product-detail-route |    1 | 16.7% |     128 B |   128 B | string:1   | 1/0            |

=== Biggest 5 keys ===
+---+----------+--------+------------+---------------------------------------+
| # | Size     | Type   | TTL        | Key                                   |
+=========================================================================...+
| 1 | 20.61 kB | string | 3597s      | WpDcVyo5fP:product-detail-route-...   |
| 2 |  5.28 kB | string | persistent | WpDcVyo5fP:cached-product-...         |
+---+----------+--------+------------+---------------------------------------+
```

`Persist/Expire` reads as *(keys with no TTL) / (keys with a TTL)* — a quick way
to spot cache that never expires.

</details>

### `get` — read a single key

The non-interactive counterpart of the TUI value inspector. It fetches one key,
applies the same type-aware decoding and automatic decompression (including
streams embedded in Symfony cache wrappers), and prints the result to stdout.

```bash
B="shopware-redis-cli-helper --url redis://127.0.0.1:6379/0"

$B get "<key>"            # decoded value to stdout (pipe-friendly)
$B get "<key>" --header   # prepend a metadata header (to stderr)
$B get "<key>" --hex      # hex dump
$B get "<key>" --json     # value + metadata as JSON
$B get 'ns:\x01tags\x01product'   # tag sets use the \x01 escape notation
```

| Flag       | Description                                                          |
|------------|----------------------------------------------------------------------|
| `--header` | Always print the metadata header (type, size, TTL, compression)      |
| `--raw`    | Never print the header (the default when output is piped)            |
| `--hex`    | Print a hex dump instead of decoded text                            |
| `--json`   | Emit a JSON object with the value and its metadata                  |

The metadata header is written to **stderr**, so a piped `get <key>` yields just
the value. A missing key exits non-zero.

### `cleanup` — prune orphaned tags

Symfony stores, for each cache tag, a Redis set of the keys carrying it. Those
keys expire on their own, but the tag sets are not pruned and accumulate members
pointing at keys that no longer exist. `cleanup` removes those orphaned members
and deletes sets that become empty.

```bash
B="shopware-redis-cli-helper --url redis://127.0.0.1:6379/0"

$B cleanup                                 # dry run (default): report only
$B cleanup --namespace WpDcVyo5fP          # scope to one namespace
$B cleanup --apply                         # perform the cleanup
$B cleanup --apply --concurrency 16 --batch-size 200
$B cleanup --json                          # machine-readable summary
```

| Flag            | Default | Description                                              |
|-----------------|---------|----------------------------------------------------------|
| `--namespace`   | (all)   | Scope to one namespace prefix; default is every namespace |
| `--apply`       | off     | Perform the cleanup (otherwise dry run)                  |
| `--concurrency` | `8`     | Parallel workers processing tag batches                  |
| `--count`       | `1000`  | `SCAN COUNT` hint for discovering tag keys               |
| `--batch-size`  | `100`   | Tag keys per Lua invocation                              |
| `--json`        | off     | Emit a JSON summary instead of text                      |

**Performance.** `cleanup` ports the optimized Lua script from FroshTools: a
single variadic `EXISTS` confirms a healthy tag has no orphans in O(1) Redis
calls regardless of set size, and only sets with missing members take the slower
per-member path. On top of that, a single `SCAN` producer feeds batches to
`--concurrency` workers, each on its own multiplexed connection, so batches are
processed in parallel.

> Redis runs Lua single-threaded, so concurrency helps most when network latency
> dominates (remote Redis); the gain is smaller over a local socket.
> `--concurrency 1` reproduces the original sequential model, and results are
> identical regardless of concurrency.

## Why it's production-safe

- It never calls `KEYS` (which blocks the server) — all iteration uses
  non-blocking `SCAN` with a configurable `COUNT`.
- Insights enrichment (`MEMORY USAGE`, `TYPE`, `TTL`) is pipelined: one network
  round-trip per `SCAN` batch, not one per key.
- `cleanup` defaults to a dry run and mutates only with `--apply`.
- The connection fails fast on a wrong host or port via `--connect-timeout`,
  rather than hanging on the OS TCP timeout.

## How keys are grouped

Shopware / Symfony cache keys look like:

```text
WpDcVyo5fP:product-detail-route-1195e812ffd3266c2ed02a5d603d26f5-481ce4e8cb0b80a09beac0aaeea37457
WpDcVyo5fP:cached-product-<hash>-<hash>-<hash>
WpDcVyo5fP:\x01tags\x01product-<hash>
```

`insights` treats the text before the first `:` as the **namespace** (a random
per-install prefix) and the remainder as the key **type**. Trailing hex-hash
segments are stripped, so the thousands of individual product caches collapse
into a single `product-detail-route` bucket.

## Development

```bash
cargo test            # unit + integration tests
cargo build --release
```

The `cleanup`, `inspect`, and value-decoding suites include integration tests
that run against a local Redis on database 15 (under an isolated key prefix).
They are skipped automatically when no Redis is reachable.

Source layout:

| File              | Responsibility                                              |
|-------------------|-------------------------------------------------------------|
| `src/main.rs`     | Global args, subcommand dispatch, connection handling       |
| `src/insights.rs` | `insights` command: `tui` / `report` subcommands            |
| `src/get.rs`      | `get` command: single-key value output                      |
| `src/cleanup.rs`  | `cleanup` command: ported Lua + concurrent runner           |
| `src/scanner.rs`  | Non-blocking `SCAN` loop and pipelined enrichment           |
| `src/stats.rs`    | Aggregation and the bounded biggest-keys heap               |
| `src/grouping.rs` | Namespace / type parsing and hash-stripping                 |
| `src/report.rs`   | Terminal-table and Markdown rendering                       |
| `src/tui.rs`      | Interactive ratatui UI: scan, navigation, drill-down        |
| `src/inspect.rs`  | Async fetcher: list a type's keys, read a value             |
| `src/value.rs`    | Compression detection and decoding for display              |

## License

Released under the [MIT License](LICENSE.md).
