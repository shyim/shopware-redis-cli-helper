//! The `cleanup` command: remove orphaned Symfony cache tags.
//!
//! Symfony's cache tagging stores, for every tag, a Redis *set* whose members
//! are the cache keys carrying that tag. Those keys expire on their own, but the
//! tag set is not pruned automatically, so over time the sets fill up with
//! members pointing at keys that no longer exist ("orphans"). This command scans
//! the tag sets and drops orphaned members, deleting sets that become empty.
//!
//! It ports the optimized Lua from FroshTools' RedisTagCleanupCommand, and adds
//! concurrency: a single SCAN producer feeds batches of tag keys to a pool of
//! workers, each running the script over its own multiplexed connection.

use crate::connect;
use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use redis::aio::ConnectionManager;
use redis::Script;
use std::sync::Arc;
use tokio::sync::mpsc;

/// The Symfony tag-set marker: keys look like `<namespace>:\x01tags\x01<name>`
/// where the `\x01` are literal 0x01 control bytes.
const TAG_INFIX: &str = "\u{0001}tags\u{0001}";

/// Optimized cleanup script (ported from RedisTagCleanupCommand.php).
///
/// KEYS = the tag set keys in this batch. ARGV[1] = "1" for dry-run.
///
/// Fast path: a single variadic EXISTS counts how many members still exist, so
/// a healthy tag (all members alive) costs O(1) Redis calls regardless of size.
/// Only when some members are missing do we fall back to per-member EXISTS to
/// find which, chunking SREM/DEL to stay under Lua's unpack limits.
const LUA_CLEANUP: &str = r#"
local dry_run = ARGV[1] == '1'
local CHUNK = 4000

local deleted_keys = 0
local removed_members = 0
local processed_keys = 0

for _, tag_key in ipairs(KEYS) do
    processed_keys = processed_keys + 1
    local members = redis.call('SMEMBERS', tag_key)
    local total = #members

    if total == 0 then
        if not dry_run then
            redis.call('DEL', tag_key)
        end
        deleted_keys = deleted_keys + 1
    else
        local alive = 0
        for i = 1, total, CHUNK do
            local args = {}
            for j = i, math.min(i + CHUNK - 1, total) do
                args[#args + 1] = members[j]
            end
            alive = alive + redis.call('EXISTS', unpack(args))
        end

        local missing = total - alive
        if missing > 0 then
            local to_remove = {}
            for _, member in ipairs(members) do
                if redis.call('EXISTS', member) == 0 then
                    to_remove[#to_remove + 1] = member
                end
            end

            if not dry_run then
                for i = 1, #to_remove, CHUNK do
                    local args = {}
                    for j = i, math.min(i + CHUNK - 1, #to_remove) do
                        args[#args + 1] = to_remove[j]
                    end
                    redis.call('SREM', tag_key, unpack(args))
                end
            end
            removed_members = removed_members + #to_remove

            if missing == total then
                if not dry_run then
                    redis.call('DEL', tag_key)
                end
                deleted_keys = deleted_keys + 1
            end
        end
    end
end

return {processed_keys, removed_members, deleted_keys}
"#;

#[derive(Args, Debug)]
pub struct CleanupArgs {
    /// Scope to a single namespace prefix (e.g. WpDcVyo5fP). Default: every
    /// namespace's tag sets (`*:\x01tags\x01*`).
    #[arg(long)]
    pub namespace: Option<String>,

    /// Actually delete orphaned members and empty sets. Without this the
    /// command runs as a DRY RUN and only reports what it would do.
    #[arg(long)]
    pub apply: bool,

    /// Number of concurrent workers processing tag batches.
    #[arg(long, default_value_t = 8)]
    pub concurrency: usize,

    /// COUNT hint for SCAN: tag keys discovered per server-side step.
    #[arg(long, default_value_t = 1000)]
    pub count: usize,

    /// Tag keys handed to each Lua invocation.
    #[arg(long, default_value_t = 100)]
    pub batch_size: usize,

    /// Emit a machine-readable JSON summary instead of text.
    #[arg(long)]
    pub json: bool,
}

/// Running totals returned by the Lua script, summed across all batches.
#[derive(Default, Clone, Copy, serde::Serialize)]
struct Totals {
    processed_keys: u64,
    removed_members: u64,
    deleted_keys: u64,
}

impl Totals {
    fn add(&mut self, t: Totals) {
        self.processed_keys += t.processed_keys;
        self.removed_members += t.removed_members;
        self.deleted_keys += t.deleted_keys;
    }
}

/// Build the SCAN MATCH pattern (with real control bytes) for the chosen scope.
fn match_pattern(namespace: Option<&str>) -> String {
    match namespace {
        Some(ns) => format!("{ns}:{TAG_INFIX}*"),
        None => format!("*{TAG_INFIX}*"),
    }
}

pub async fn run(args: CleanupArgs, client: redis::Client, connect_timeout: u64) -> Result<()> {
    let pattern = match_pattern(args.namespace.as_deref());
    let dry_run = !args.apply;
    let concurrency = args.concurrency.max(1);

    if !args.json {
        let scope = args
            .namespace
            .as_deref()
            .map(|n| format!("namespace '{n}'"))
            .unwrap_or_else(|| "all namespaces".to_string());
        eprintln!(
            "Redis tag cleanup — {scope}{}",
            if dry_run { " (DRY RUN)" } else { "" }
        );
        if dry_run {
            eprintln!("No keys will be modified. Re-run with --apply to clean.");
        }
    }

    // The producer SCANs; workers consume batches. One multiplexed connection
    // per worker (cheap clones of the manager) gives true parallelism.
    let mut producer_conn = connect(client.clone(), connect_timeout).await?;
    let script = Arc::new(Script::new(LUA_CLEANUP));

    let (tx, rx) = mpsc::channel::<Vec<Vec<u8>>>(concurrency * 2);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {pos} tag keys processed {msg}").unwrap(),
    );
    let pb = Arc::new(pb);

    // Spawn workers.
    let mut workers = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let mut conn = connect(client.clone(), connect_timeout).await?;
        let rx = rx.clone();
        let script = script.clone();
        let pb = pb.clone();
        workers.push(tokio::spawn(async move {
            let mut totals = Totals::default();
            loop {
                let batch = {
                    let mut guard = rx.lock().await;
                    guard.recv().await
                };
                let Some(batch) = batch else { break };
                match run_batch(&mut conn, &script, &batch, dry_run).await {
                    Ok(t) => {
                        totals.add(t);
                        pb.inc(batch.len() as u64);
                    }
                    Err(e) => {
                        // Report and keep going; one bad batch shouldn't abort all.
                        pb.println(format!("batch error: {e}"));
                    }
                }
            }
            totals
        }));
    }

    // Producer: SCAN the keyspace, push fixed-size batches into the channel.
    let mut cursor: u64 = 0;
    let mut pending: Vec<Vec<u8>> = Vec::with_capacity(args.batch_size);
    loop {
        let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern.as_bytes())
            .arg("COUNT")
            .arg(args.count)
            .query_async(&mut producer_conn)
            .await
            .context("SCAN failed")?;

        for k in keys {
            pending.push(k);
            if pending.len() >= args.batch_size {
                let batch = std::mem::replace(&mut pending, Vec::with_capacity(args.batch_size));
                if tx.send(batch).await.is_err() {
                    break;
                }
            }
        }

        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    if !pending.is_empty() {
        let _ = tx.send(pending).await;
    }
    drop(tx); // close the channel so workers finish

    // Collect worker totals.
    let mut totals = Totals::default();
    for w in workers {
        if let Ok(t) = w.await {
            totals.add(t);
        }
    }
    pb.finish_and_clear();

    report(&args, dry_run, totals);
    Ok(())
}

/// Run the cleanup script over one batch of tag keys.
async fn run_batch(
    conn: &mut ConnectionManager,
    script: &Script,
    keys: &[Vec<u8>],
    dry_run: bool,
) -> Result<Totals> {
    let mut inv = script.prepare_invoke();
    for k in keys {
        inv.key(k.as_slice());
    }
    inv.arg(if dry_run { "1" } else { "0" });

    let (processed_keys, removed_members, deleted_keys): (u64, u64, u64) = inv
        .invoke_async(conn)
        .await
        .context("cleanup script failed")?;
    Ok(Totals {
        processed_keys,
        removed_members,
        deleted_keys,
    })
}

fn report(args: &CleanupArgs, dry_run: bool, t: Totals) {
    if args.json {
        #[derive(serde::Serialize)]
        struct Out {
            dry_run: bool,
            #[serde(flatten)]
            totals: Totals,
        }
        let out = Out { dry_run, totals: t };
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }

    println!();
    println!("Tag keys processed:  {}", t.processed_keys);
    if dry_run {
        println!("Orphaned members:    {} (would remove)", t.removed_members);
        println!("Empty tag sets:      {} (would delete)", t.deleted_keys);
        println!("\nDry run complete — re-run with --apply to perform the cleanup.");
    } else {
        println!("Orphaned members removed: {}", t.removed_members);
        println!("Empty tag sets deleted:   {}", t.deleted_keys);
        println!("\nCleanup complete.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_scopes_to_namespace_with_control_bytes() {
        let all = match_pattern(None);
        assert_eq!(all, "*\u{0001}tags\u{0001}*");
        let scoped = match_pattern(Some("WpDcVyo5fP"));
        assert_eq!(scoped, "WpDcVyo5fP:\u{0001}tags\u{0001}*");
    }

    #[test]
    fn totals_accumulate() {
        let mut a = Totals::default();
        a.add(Totals {
            processed_keys: 2,
            removed_members: 5,
            deleted_keys: 1,
        });
        a.add(Totals {
            processed_keys: 3,
            removed_members: 0,
            deleted_keys: 2,
        });
        assert_eq!(a.processed_keys, 5);
        assert_eq!(a.removed_members, 5);
        assert_eq!(a.deleted_keys, 3);
    }

    // ---- Integration tests against a real Redis (db 15) ----
    //
    // Skipped automatically when no Redis is reachable on localhost, so the
    // suite stays green in environments without one. Each test isolates itself
    // under a unique namespace prefix and cleans up after itself.

    const TEST_URL: &str = "redis://127.0.0.1:6379/15";

    async fn try_conn() -> Option<ConnectionManager> {
        let client = redis::Client::open(TEST_URL).ok()?;
        connect(client, 1).await.ok()
    }

    /// SCARD that returns 0 for a missing key.
    async fn scard(conn: &mut ConnectionManager, key: &str) -> i64 {
        redis::cmd("SCARD")
            .arg(key)
            .query_async(conn)
            .await
            .unwrap_or(0)
    }

    async fn exists(conn: &mut ConnectionManager, key: &str) -> bool {
        redis::cmd("EXISTS")
            .arg(key)
            .query_async::<i64>(conn)
            .await
            .unwrap_or(0)
            == 1
    }

    /// Seed a healthy/partial/dead scenario under `ns` and an untouched set
    /// under `other_ns`. Returns the tag-set key names.
    async fn seed(conn: &mut ConnectionManager, ns: &str, other_ns: &str) {
        let _: () = redis::cmd("SET")
            .arg(format!("{ns}:alive-1"))
            .arg("v")
            .query_async(conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(format!("{ns}:alive-2"))
            .arg("v")
            .query_async(conn)
            .await
            .unwrap();

        // healthy: both members alive
        let _: () = redis::cmd("SADD")
            .arg(format!("{ns}:{TAG_INFIX}healthy"))
            .arg(format!("{ns}:alive-1"))
            .arg(format!("{ns}:alive-2"))
            .query_async(conn)
            .await
            .unwrap();
        // partial: one alive, one orphan
        let _: () = redis::cmd("SADD")
            .arg(format!("{ns}:{TAG_INFIX}partial"))
            .arg(format!("{ns}:alive-1"))
            .arg(format!("{ns}:gone-9"))
            .query_async(conn)
            .await
            .unwrap();
        // dead: all orphans
        let _: () = redis::cmd("SADD")
            .arg(format!("{ns}:{TAG_INFIX}dead"))
            .arg(format!("{ns}:gone-1"))
            .arg(format!("{ns}:gone-2"))
            .query_async(conn)
            .await
            .unwrap();
        // other namespace, all orphans (must stay untouched when scoped)
        let _: () = redis::cmd("SADD")
            .arg(format!("{other_ns}:{TAG_INFIX}dead"))
            .arg(format!("{other_ns}:gone-1"))
            .query_async(conn)
            .await
            .unwrap();
    }

    async fn cleanup_keys(conn: &mut ConnectionManager, prefixes: &[&str]) {
        for p in prefixes {
            // DEL everything matching the prefix via a scoped scan.
            let mut cursor = 0u64;
            loop {
                let (next, keys): (u64, Vec<Vec<u8>>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(format!("{p}*").as_bytes())
                    .arg("COUNT")
                    .arg(1000)
                    .query_async(conn)
                    .await
                    .unwrap();
                for k in keys {
                    let _: () = redis::cmd("DEL").arg(k).query_async(conn).await.unwrap();
                }
                cursor = next;
                if cursor == 0 {
                    break;
                }
            }
        }
    }

    fn args(namespace: Option<&str>, apply: bool, concurrency: usize) -> CleanupArgs {
        CleanupArgs {
            namespace: namespace.map(String::from),
            apply,
            concurrency,
            count: 1000,
            batch_size: 100,
            json: true,
        }
    }

    #[tokio::test]
    async fn dry_run_reports_but_does_not_mutate() {
        let Some(mut conn) = try_conn().await else {
            eprintln!("skipping: no Redis on {TEST_URL}");
            return;
        };
        let ns = "swrci-test-dry";
        let other = "swrci-test-dry-other";
        cleanup_keys(&mut conn, &[ns, other]).await;
        seed(&mut conn, ns, other).await;

        let client = redis::Client::open(TEST_URL).unwrap();
        run(args(Some(ns), false, 4), client, 2).await.unwrap();

        // Nothing changed: dead set still present, partial still has its orphan.
        assert!(exists(&mut conn, &format!("{ns}:{TAG_INFIX}dead")).await);
        assert_eq!(
            scard(&mut conn, &format!("{ns}:{TAG_INFIX}partial")).await,
            2
        );

        cleanup_keys(&mut conn, &[ns, other]).await;
    }

    #[tokio::test]
    async fn apply_removes_orphans_and_respects_namespace() {
        let Some(mut conn) = try_conn().await else {
            eprintln!("skipping: no Redis on {TEST_URL}");
            return;
        };
        let ns = "swrci-test-apply";
        let other = "swrci-test-apply-other";
        cleanup_keys(&mut conn, &[ns, other]).await;
        seed(&mut conn, ns, other).await;

        let client = redis::Client::open(TEST_URL).unwrap();
        run(args(Some(ns), true, 8), client, 2).await.unwrap();

        // dead set deleted, healthy kept intact, partial pruned to its 1 alive.
        assert!(!exists(&mut conn, &format!("{ns}:{TAG_INFIX}dead")).await);
        assert_eq!(
            scard(&mut conn, &format!("{ns}:{TAG_INFIX}healthy")).await,
            2
        );
        assert_eq!(
            scard(&mut conn, &format!("{ns}:{TAG_INFIX}partial")).await,
            1
        );
        // The other namespace's dead set must be untouched (scoping).
        assert!(exists(&mut conn, &format!("{other}:{TAG_INFIX}dead")).await);

        cleanup_keys(&mut conn, &[ns, other]).await;
    }

    #[tokio::test]
    async fn concurrency_does_not_change_results() {
        let Some(mut conn) = try_conn().await else {
            eprintln!("skipping: no Redis on {TEST_URL}");
            return;
        };
        // Run apply twice on identical seeds with different concurrency; the
        // resulting DB state must match.
        for (ns, conc) in [("swrci-test-conc1", 1usize), ("swrci-test-conc8", 8usize)] {
            let other = "swrci-test-conc-other";
            cleanup_keys(&mut conn, &[ns, other]).await;
            seed(&mut conn, ns, other).await;
            let client = redis::Client::open(TEST_URL).unwrap();
            run(args(Some(ns), true, conc), client, 2).await.unwrap();

            assert!(!exists(&mut conn, &format!("{ns}:{TAG_INFIX}dead")).await);
            assert_eq!(
                scard(&mut conn, &format!("{ns}:{TAG_INFIX}partial")).await,
                1
            );
            assert_eq!(
                scard(&mut conn, &format!("{ns}:{TAG_INFIX}healthy")).await,
                2
            );
            cleanup_keys(&mut conn, &[ns, other]).await;
        }
    }
}
