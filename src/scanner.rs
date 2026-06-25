//! The SCAN driver. Iterates the whole keyspace without ever calling KEYS
//! (which would block Redis), optionally enriching each batch with MEMORY
//! USAGE / TYPE / TTL via a single pipeline round-trip per batch.

use crate::grouping::parse_key;
use crate::stats::{KeyObservation, Stats};
use anyhow::{Context, Result};
use redis::aio::ConnectionManager;

#[derive(Clone, Copy)]
pub struct ScanOptions {
    /// COUNT hint for SCAN — keys per server-side step.
    pub count: usize,
    /// Treat first N chars as namespace when a key has no ':'.
    pub namespace_len: usize,
    /// Stop after this many keys (0 = unlimited). Used for sampling.
    pub limit: u64,
    pub want_memory: bool,
    pub want_types: bool,
    pub want_ttl: bool,
    /// Retain this many largest keys (by memory). 0 disables the leaderboard.
    pub biggest: usize,
}

/// Scan the entire keyspace, returning aggregated stats.
///
/// `on_progress` is invoked after every SCAN batch with the stats accumulated
/// so far. Callers use it to drive a progress bar (CLI) or to publish live
/// snapshots into shared state (TUI). It is a no-op for the simplest callers.
pub async fn scan<F>(
    conn: &mut ConnectionManager,
    match_pattern: Option<&str>,
    opts: ScanOptions,
    mut on_progress: F,
) -> Result<Stats>
where
    F: FnMut(&Stats),
{
    let mut stats = Stats::with_biggest_cap(opts.biggest);

    let mut cursor: u64 = 0;
    loop {
        // SCAN cursor [MATCH pattern] COUNT count
        let mut cmd = redis::cmd("SCAN");
        cmd.arg(cursor).arg("COUNT").arg(opts.count);
        if let Some(pat) = match_pattern {
            cmd.arg("MATCH").arg(pat);
        }

        let (next, batch): (u64, Vec<String>) =
            cmd.query_async(conn).await.context("SCAN command failed")?;

        if !batch.is_empty() {
            let observations = enrich(conn, &batch, opts).await?;
            for (key, obs) in batch.iter().zip(observations) {
                let parsed = parse_key(key, opts.namespace_len);
                stats.record(key, &parsed.namespace, &parsed.key_type, &obs);
            }
            on_progress(&stats);
        }

        cursor = next;
        if cursor == 0 {
            break;
        }
        if opts.limit != 0 && stats.total_keys >= opts.limit {
            break;
        }
    }

    Ok(stats)
}

/// Build one observation per key. When no enrichment is requested this is a
/// cheap no-op. Otherwise all extra commands for the whole batch go out in a
/// single pipeline so we pay one network round-trip per SCAN batch, not one
/// per key.
async fn enrich(
    conn: &mut ConnectionManager,
    keys: &[String],
    opts: ScanOptions,
) -> Result<Vec<KeyObservation>> {
    let mut observations: Vec<KeyObservation> =
        keys.iter().map(|_| KeyObservation::default()).collect();

    if !opts.want_memory && !opts.want_types && !opts.want_ttl {
        return Ok(observations);
    }

    let mut pipe = redis::pipe();
    for key in keys {
        if opts.want_memory {
            pipe.cmd("MEMORY").arg("USAGE").arg(key);
        }
        if opts.want_types {
            pipe.cmd("TYPE").arg(key);
        }
        if opts.want_ttl {
            pipe.cmd("TTL").arg(key);
        }
    }

    // The pipeline returns replies in command order. Decode generically so a
    // nil MEMORY USAGE (key gone) doesn't abort the whole batch.
    let replies: Vec<redis::Value> = pipe
        .query_async(conn)
        .await
        .context("enrichment pipeline failed")?;

    let mut idx = 0;
    for obs in observations.iter_mut() {
        if opts.want_memory {
            obs.bytes = match replies.get(idx) {
                Some(redis::Value::Int(n)) if *n >= 0 => Some(*n as u64),
                _ => None,
            };
            idx += 1;
        }
        if opts.want_types {
            obs.data_type = match replies.get(idx) {
                Some(redis::Value::SimpleString(s)) => Some(s.clone()),
                Some(redis::Value::BulkString(b)) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            };
            idx += 1;
        }
        if opts.want_ttl {
            obs.ttl = match replies.get(idx) {
                Some(redis::Value::Int(n)) => Some(*n),
                _ => None,
            };
            idx += 1;
        }
    }

    Ok(observations)
}
