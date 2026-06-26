//! Live server stats for the TUI header.
//!
//! A small poller task runs `INFO` + `DBSIZE` on its own connection every few
//! seconds and publishes a parsed snapshot, so the header can show how full
//! Redis is (and eviction pressure) updating live while the user browses.

use redis::aio::ConnectionManager;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the poller refreshes. Short enough to feel live, long enough to be
/// negligible load (one INFO + one DBSIZE per tick).
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A parsed snapshot of the relevant `INFO` fields plus `DBSIZE`.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub redis_version: Option<String>,
    pub used_memory: Option<u64>,
    /// 0 means no limit (the server has no configured ceiling).
    pub maxmemory: Option<u64>,
    pub maxmemory_policy: Option<String>,
    pub evicted_keys: Option<u64>,
    pub mem_fragmentation_ratio: Option<f64>,
    pub connected_clients: Option<u64>,
    /// Number of keys in the scanned DB (`DBSIZE`).
    pub db_keys: Option<u64>,
}

impl ServerInfo {
    /// Fraction of the memory ceiling in use, if a ceiling is configured.
    /// `None` when `maxmemory` is 0/unset (unlimited).
    pub fn memory_fullness(&self) -> Option<f64> {
        match (self.used_memory, self.maxmemory) {
            (Some(used), Some(max)) if max > 0 => Some((used as f64 / max as f64).min(1.0)),
            _ => None,
        }
    }
}

/// Shared slot the poller writes and the UI reads. `None` until the first
/// successful poll (or if the server is unreachable).
pub type InfoHandle = Arc<Mutex<Option<ServerInfo>>>;

/// Parse the textual `INFO` reply. Lines are `key:value`; sections start with
/// `#`. We pick out only the fields we display.
pub fn parse_info(text: &str) -> ServerInfo {
    let mut info = ServerInfo::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        match key {
            "redis_version" => info.redis_version = Some(val.to_string()),
            "used_memory" => info.used_memory = val.parse().ok(),
            "maxmemory" => info.maxmemory = val.parse().ok(),
            "maxmemory_policy" => info.maxmemory_policy = Some(val.to_string()),
            "evicted_keys" => info.evicted_keys = val.parse().ok(),
            "mem_fragmentation_ratio" => info.mem_fragmentation_ratio = val.parse().ok(),
            "connected_clients" => info.connected_clients = val.parse().ok(),
            _ => {}
        }
    }
    info
}

/// Spawn the poller. It connects lazily (one extra connection) and refreshes the
/// shared snapshot every [`POLL_INTERVAL`] until the returned task is aborted.
pub fn spawn(
    client: redis::Client,
    connect_timeout: u64,
    handle: InfoHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut conn: Option<ConnectionManager> = None;
        loop {
            if conn.is_none() {
                conn = crate::connect(client.clone(), connect_timeout).await.ok();
            }
            if let Some(c) = conn.as_mut() {
                if let Some(info) = poll_once(c).await {
                    *handle.lock().unwrap() = Some(info);
                } else {
                    // The connection went bad; drop it so we reconnect next tick.
                    conn = None;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
}

async fn poll_once(conn: &mut ConnectionManager) -> Option<ServerInfo> {
    let raw: String = redis::cmd("INFO").query_async(conn).await.ok()?;
    let mut info = parse_info(&raw);
    // DBSIZE is the current DB's key count (the one the scan ran against).
    info.db_keys = redis::cmd("DBSIZE").query_async(conn).await.ok();
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Server
redis_version:7.2.4
uptime_in_seconds:263467
# Clients
connected_clients:4
# Memory
used_memory:6073440
used_memory_human:5.79M
maxmemory:104857600
maxmemory_policy:allkeys-lru
mem_fragmentation_ratio:3.22
# Stats
evicted_keys:128
";

    #[test]
    fn parses_relevant_fields() {
        let i = parse_info(SAMPLE);
        assert_eq!(i.redis_version.as_deref(), Some("7.2.4"));
        assert_eq!(i.used_memory, Some(6_073_440));
        assert_eq!(i.maxmemory, Some(104_857_600));
        assert_eq!(i.maxmemory_policy.as_deref(), Some("allkeys-lru"));
        assert_eq!(i.evicted_keys, Some(128));
        assert_eq!(i.connected_clients, Some(4));
        assert_eq!(i.mem_fragmentation_ratio, Some(3.22));
    }

    #[test]
    fn memory_fullness_computed_when_capped() {
        let i = parse_info(SAMPLE);
        let pct = i.memory_fullness().unwrap();
        // 6073440 / 104857600 ≈ 0.0579
        assert!((pct - 0.0579).abs() < 0.001, "got {pct}");
    }

    #[test]
    fn memory_fullness_none_when_unlimited() {
        let i = parse_info("used_memory:1000\nmaxmemory:0\n");
        assert_eq!(i.memory_fullness(), None);
    }

    #[test]
    fn ignores_section_headers_and_blanks() {
        let i = parse_info("# Server\n\nredis_version:1.2.3\n\n");
        assert_eq!(i.redis_version.as_deref(), Some("1.2.3"));
    }
}
