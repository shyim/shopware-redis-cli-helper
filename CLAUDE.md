# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`shopware-redis-cli-helper` is a Rust CLI for inspecting and maintaining a
Shopware / Symfony Redis cache. It has four subcommands: `insights` (scan +
explore, with `tui` and `report` modes), `get` (read one key's decoded value),
and `cleanup` (prune orphaned Symfony cache tags). The README is the
user-facing reference for command behaviour and flags.

## Commands

```bash
cargo build --release          # binary at ./target/release/shopware-redis-cli-helper
cargo test                     # unit + integration tests
cargo test value::tests::detects_and_inflates_gzip   # a single test by path
cargo test inspect             # all tests whose name contains "inspect"
cargo fmt --all                # format (CI gate: `cargo fmt --all --check`)
cargo clippy --all-targets --all-features -- -D warnings   # lints as errors (CI gate)
```

CI (`.github/workflows/ci.yml`) runs **fmt-check, clippy `-D warnings`, and
tests** on Linux with a Redis 7 service container. Before considering a change
done, run all three locally — clippy with `-D warnings` is strict and will fail
CI on lints that `cargo build` ignores.

### Integration tests need a local Redis

Several test modules (`cleanup`, `inspect`, `value`) connect to a hardcoded
`redis://127.0.0.1:6379/15` (the `TEST_URL` constant — db **15**, an isolated
prefix). They **skip silently when no Redis is reachable**, so a green
`cargo test` with no Redis running does *not* mean the integration paths were
exercised. Run a local Redis to actually test them. They clean up after
themselves but use db 15 — don't point them at a database you care about.

## Architecture

The crate is a single binary (`src/main.rs`) that parses global args (`--url`,
`--connect-timeout`) and dispatches to one command module. `connect()` in
`main.rs` is the shared connection helper — it wraps `ConnectionManager` with a
bounded timeout and a single retry so a wrong host/port **fails fast** instead
of hanging on the OS TCP timeout. All commands go through it.

### Scanning pipeline (`scanner.rs` → `stats.rs` → `grouping.rs`)

`scanner::scan` is the one keyspace iterator, shared by `insights tui`,
`insights report`, and used as the model the cleanup mirrors. It takes a
**progress callback** (`FnMut(&Stats)`) invoked after each SCAN batch — this is
the seam that lets the same scan drive an indicatif spinner (report) or publish
live snapshots into TUI shared state. It never calls `KEYS`; enrichment
(`MEMORY USAGE` / `TYPE` / `TTL`) is **pipelined** as one round-trip per batch.

- `grouping::parse_key` splits a key into `(namespace, type)`: namespace is the
  text before the first `:`; the type is the remainder with trailing hex-hash
  segments stripped, so thousands of per-entity caches collapse into one bucket.
  The Symfony tag marker (literal `0x01` byte) is normalised to the readable
  token `\x01`. This `\x01` ⇄ `0x01` conversion recurs throughout — the stored
  type uses the escaped form, but anything sent back to Redis (SCAN MATCH
  patterns, key fetches) must restore the real control bytes.
- `stats::Stats` aggregates counts/memory/types/TTL per `(namespace, type)`, and
  keeps a **bounded min-heap** of the N biggest keys so memory stays constant
  regardless of keyspace size — the full key set is never buffered.

### TUI threading model (`tui.rs` + `insights.rs`)

This is the most intricate part. The synchronous ratatui event loop runs on a
`spawn_blocking` thread; everything async lives elsewhere and communicates
through shared state the UI polls each frame:

- The **scan task** (tokio) writes `Stats` snapshots into `Arc<Mutex<tui::Shared>>`.
- The **scan-depth picker** lives inside the TUI: a `oneshot::Sender<bool>` is
  handed to the UI; the scan task `await`s the receiver before connecting, so
  nothing heavy runs until the user picks basic vs advanced (or a `--mode`/stat
  flag presets it and the sender fires immediately).
- The **drill-down fetcher** (`inspect::spawn`) is a separate tokio task with its
  own lazily-created connection. The UI sends `Request`s over an mpsc channel;
  results land in `Arc<Mutex<inspect::Inbox>>` which the UI polls via
  `poll_inbox`, using a `generation` counter to detect fresh replies and a
  screen-aware check so a late reply can't yank the user out of where they
  navigated.

The TUI is the default on a terminal; it's skipped when stdout is piped or a
non-interactive output flag is set.

### Value decoding (`value.rs`, `inspect.rs`, `get.rs`)

`value::decode` is shared by the TUI inspector and the `get` command. Decoding
order matters and is non-obvious: (1) try leading-byte decompression, (2) if
that's plain, **scan the payload for an *embedded* compression stream** —
Symfony wraps cache items in a binary header + serialized tag envelope with the
real value compressed *inside* (commonly a zlib stream at a non-zero offset), so
leading-byte sniffing alone misses it. It returns **both** a text and a hex
rendering plus `is_binary`, so the UI can toggle (`x`) without re-fetching.
`inspect::get_value` is the type-aware reader (string vs set/list/hash/zset) and
is reused by `get` with a larger display cap.

### Cleanup (`cleanup.rs`)

Ports the optimised Lua from FroshTools' `RedisTagCleanupCommand` (single
variadic `EXISTS` for the healthy-tag fast path). Adds concurrency: one SCAN
producer feeds tag-key batches over a channel to N workers, each with its own
cloned multiplexed connection, running the script via cached EVALSHA. The
`dry_run` flag (ARGV[1]) gates all mutations so dry-run and `--apply` share one
code path. **Dry run is the default**; mutation requires `--apply`.

## Testing approach for the TUI

Don't rely on PTY screen-scraping to verify TUI rendering — ratatui's
cursor-addressed output collapses under naive escape-stripping and produces
false negatives. Instead use ratatui's `TestBackend` to render a frame into an
in-memory buffer and assert on its cell content (see the render tests in
`tui.rs`). Navigation/state logic is tested by driving `handle_key` directly.

## Release

Tagging `v*` triggers `.github/workflows/release.yml` (goreleaser +
cargo-zigbuild) to build macOS/Linux × amd64/arm64 archives. `.goreleaser.yaml`
defines the targets and archive naming.
