//! On-demand drill-down: list a type's keys, and inspect a key's value.
//!
//! The TUI runs on a blocking thread with no Redis access of its own, so it
//! sends requests over a channel to an async `fetcher` task that owns a live
//! connection. Results are written back into shared state the UI polls.

use crate::value::{self, Decoded};
use redis::aio::ConnectionManager;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// How many keys a drill-down lists at most (a sample, not the full set).
pub const KEY_SAMPLE_CAP: usize = 1000;
/// How many characters / bytes of a value we render.
pub const VALUE_DISPLAY_CAP: usize = 20_000;
/// How many elements of a collection (set/hash/list/zset) we show.
pub const COLLECTION_CAP: usize = 500;

/// A request from the UI to the fetcher.
#[derive(Debug)]
pub enum Request {
    /// List sample keys belonging to a (namespace, type).
    ListKeys { namespace: String, key_type: String },
    /// Fetch and decode a single key's value.
    GetValue { key: String },
}

/// Result of a key listing.
#[derive(Debug, Clone, Default)]
pub struct KeyList {
    pub namespace: String,
    pub key_type: String,
    pub keys: Vec<String>,
    /// True if the sample cap was hit (more keys exist than shown).
    pub capped: bool,
    pub error: Option<String>,
}

/// Result of a value fetch.
#[derive(Debug, Clone)]
pub struct KeyValue {
    pub key: String,
    pub redis_type: String,
    pub ttl: Option<i64>,
    pub size_bytes: Option<u64>,
    /// Number of elements for a collection type (None for strings/missing).
    pub elements: Option<u64>,
    /// Text rendering of the body (decoded string, or a collection listing).
    pub body: String,
    /// Hex rendering of a string value (None for collections). Lets the UI
    /// toggle between text and hex without re-fetching.
    pub hex: Option<String>,
    /// True when the string value is binary (UI defaults to the hex view).
    pub is_binary: bool,
    /// Compression encoding detected for a string value.
    pub encoding: Option<value::Encoding>,
    /// Byte offset an embedded compression stream started at (`Some(n)`, n>0)
    /// when the value was a wrapper around compressed inner data.
    pub stream_offset: Option<usize>,
    pub error: Option<String>,
}

/// Shared slot the fetcher writes into and the UI reads. `generation` bumps on
/// every new result so the UI can tell a fresh reply from a stale one.
#[derive(Default)]
pub struct Inbox {
    pub key_list: Option<KeyList>,
    pub key_value: Option<KeyValue>,
    /// True while a request is in flight (UI shows a "loading…" hint).
    pub loading: bool,
    pub generation: u64,
}

pub type InboxHandle = Arc<Mutex<Inbox>>;

/// Format a TTL for a single key: the remaining time plus, in parentheses, the
/// absolute local date/time it expires — so you don't have to add it up.
/// `-1` = no expiry, `-2`/None = missing.
pub fn ttl_with_expiry(ttl: Option<i64>) -> String {
    match ttl {
        Some(-1) => "persistent".to_string(),
        Some(secs) if secs >= 0 => {
            let when = chrono::Local::now() + chrono::Duration::seconds(secs);
            format!("{}s (expires {})", secs, when.format("%Y-%m-%d %H:%M:%S"))
        }
        _ => "-".to_string(),
    }
}

/// Reconstruct the SCAN MATCH pattern for a (namespace, type). The stored type
/// is a normalized prefix (hashes stripped) with the tag marker escaped as
/// `\x01tags\x01`; we restore the real control bytes for matching.
pub fn match_pattern_for(namespace: &str, key_type: &str) -> String {
    let body = key_type.replace("\\x01", "\u{0001}");
    // "(no-type)" and "(hash)" buckets can't be matched precisely; fall back to
    // the namespace so the user at least sees that namespace's keys.
    if body == "(no-type)" {
        return format!("{namespace}:*");
    }
    if let Some(stripped) = body.strip_suffix("(hash)") {
        return format!("{namespace}:{stripped}*");
    }
    format!("{namespace}:{body}*")
}

/// Spawn the fetcher task. It connects lazily on the first request (so startup
/// pays for one connection, not two) and reuses that connection thereafter.
/// Returns a sender the UI uses to enqueue requests.
pub fn spawn(
    client: redis::Client,
    connect_timeout: u64,
    inbox: InboxHandle,
) -> mpsc::Sender<Request> {
    let (tx, mut rx) = mpsc::channel::<Request>(8);
    tokio::spawn(async move {
        let mut conn: Option<ConnectionManager> = None;
        while let Some(req) = rx.recv().await {
            {
                let mut g = inbox.lock().unwrap();
                g.loading = true;
            }

            // Establish (or reuse) the connection.
            if conn.is_none() {
                match crate::connect(client.clone(), connect_timeout).await {
                    Ok(c) => conn = Some(c),
                    Err(e) => {
                        let mut g = inbox.lock().unwrap();
                        report_error(&mut g, &req, e.to_string());
                        g.loading = false;
                        g.generation += 1;
                        continue;
                    }
                }
            }
            let conn = conn.as_mut().unwrap();

            match req {
                Request::ListKeys {
                    namespace,
                    key_type,
                } => {
                    let result = list_keys(conn, &namespace, &key_type).await;
                    let mut g = inbox.lock().unwrap();
                    g.key_list = Some(result);
                    g.loading = false;
                    g.generation += 1;
                }
                Request::GetValue { key } => {
                    let result = get_value(conn, &key, VALUE_DISPLAY_CAP).await;
                    let mut g = inbox.lock().unwrap();
                    g.key_value = Some(result);
                    g.loading = false;
                    g.generation += 1;
                }
            }
        }
    });
    tx
}

/// Surface a connection error against whichever result slot the request targets.
fn report_error(inbox: &mut Inbox, req: &Request, err: String) {
    match req {
        Request::ListKeys {
            namespace,
            key_type,
        } => {
            inbox.key_list = Some(KeyList {
                namespace: namespace.clone(),
                key_type: key_type.clone(),
                error: Some(err),
                ..Default::default()
            });
        }
        Request::GetValue { key } => {
            inbox.key_value = Some(KeyValue {
                key: key.clone(),
                redis_type: "unknown".into(),
                ttl: None,
                size_bytes: None,
                elements: None,
                body: String::new(),
                hex: None,
                is_binary: false,
                encoding: None,
                stream_offset: None,
                error: Some(err),
            });
        }
    }
}

async fn list_keys(conn: &mut ConnectionManager, namespace: &str, key_type: &str) -> KeyList {
    let pattern = match_pattern_for(namespace, key_type);
    let mut keys: Vec<String> = Vec::new();
    let mut cursor: u64 = 0;
    let mut capped = false;

    loop {
        let res: redis::RedisResult<(u64, Vec<Vec<u8>>)> = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern.as_bytes())
            .arg("COUNT")
            .arg(500)
            .query_async(conn)
            .await;

        let (next, batch) = match res {
            Ok(v) => v,
            Err(e) => {
                return KeyList {
                    namespace: namespace.to_string(),
                    key_type: key_type.to_string(),
                    keys,
                    capped,
                    error: Some(e.to_string()),
                }
            }
        };

        for k in batch {
            keys.push(escape_key(&k));
            if keys.len() >= KEY_SAMPLE_CAP {
                capped = true;
                break;
            }
        }

        cursor = next;
        if cursor == 0 || capped {
            break;
        }
    }

    keys.sort();
    KeyList {
        namespace: namespace.to_string(),
        key_type: key_type.to_string(),
        keys,
        capped,
        error: None,
    }
}

/// Render control bytes in a key name readably (mirrors the TUI's tag display).
fn escape_key(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\u{0001}', "\\x01")
}

/// Undo `escape_key` so we send the real bytes back to Redis.
fn unescape_key(key: &str) -> Vec<u8> {
    key.replace("\\x01", "\u{0001}").into_bytes()
}

/// Fetch and decode a single key's value. `cap` bounds the rendered body length
/// (the TUI uses a display cap; the `get` command passes a large/unbounded cap).
pub async fn get_value(conn: &mut ConnectionManager, key: &str, cap: usize) -> KeyValue {
    let raw_key = unescape_key(key);

    let redis_type: String = redis::cmd("TYPE")
        .arg(&raw_key)
        .query_async(conn)
        .await
        .unwrap_or_else(|_| "unknown".into());

    let ttl: Option<i64> = redis::cmd("TTL").arg(&raw_key).query_async(conn).await.ok();
    let size_bytes: Option<u64> = redis::cmd("MEMORY")
        .arg("USAGE")
        .arg(&raw_key)
        .query_async(conn)
        .await
        .ok()
        .flatten();

    let mut out = KeyValue {
        key: key.to_string(),
        redis_type: redis_type.clone(),
        ttl,
        size_bytes,
        elements: None,
        body: String::new(),
        hex: None,
        is_binary: false,
        encoding: None,
        stream_offset: None,
        error: None,
    };

    match redis_type.as_str() {
        "string" => {
            let raw: redis::RedisResult<Vec<u8>> =
                redis::cmd("GET").arg(&raw_key).query_async(conn).await;
            match raw {
                Ok(bytes) => {
                    let decoded: Decoded = value::decode(&bytes, cap);
                    out.encoding = Some(decoded.encoding);
                    out.stream_offset = decoded.stream_offset;
                    // After (embedded) decompression the inner data is usually
                    // text even if the wrapper made the raw value look binary.
                    out.is_binary = decoded.is_binary;
                    // Keep both renderings; the UI picks (and can toggle).
                    out.body = decoded.text;
                    out.hex = Some(decoded.hex);
                }
                Err(e) => out.error = Some(e.to_string()),
            }
        }
        "set" => collection(conn, &raw_key, "SMEMBERS", &mut out, false, cap).await,
        "list" => collection(conn, &raw_key, "LRANGE", &mut out, true, cap).await,
        "hash" => hash(conn, &raw_key, &mut out, cap).await,
        "zset" => zset(conn, &raw_key, &mut out, cap).await,
        "none" => out.error = Some("key does not exist (it may have expired)".into()),
        other => out.error = Some(format!("unsupported type: {other}")),
    }

    out
}

/// The element cap for a collection given a body-character `cap`: scale roughly
/// with the cap, but never below the display default.
fn coll_cap(cap: usize) -> usize {
    (cap / 32).max(COLLECTION_CAP)
}

async fn collection(
    conn: &mut ConnectionManager,
    raw_key: &[u8],
    cmd: &str,
    out: &mut KeyValue,
    is_list: bool,
    cap: usize,
) {
    let limit = coll_cap(cap);
    let mut c = redis::cmd(cmd);
    c.arg(raw_key);
    if is_list {
        c.arg(0).arg(limit as i64 - 1);
    }
    let members: redis::RedisResult<Vec<Vec<u8>>> = c.query_async(conn).await;
    match members {
        Ok(items) => {
            out.elements = Some(items.len() as u64);
            out.body = items
                .iter()
                .take(limit)
                .map(|m| String::from_utf8_lossy(m).replace('\u{0001}', "\\x01"))
                .collect::<Vec<_>>()
                .join("\n");
            if items.len() > limit {
                out.body
                    .push_str(&format!("\n… {} more (truncated)", items.len() - limit));
            }
        }
        Err(e) => out.error = Some(e.to_string()),
    }
}

async fn hash(conn: &mut ConnectionManager, raw_key: &[u8], out: &mut KeyValue, cap: usize) {
    let limit = coll_cap(cap);
    let pairs: redis::RedisResult<Vec<Vec<u8>>> =
        redis::cmd("HGETALL").arg(raw_key).query_async(conn).await;
    match pairs {
        Ok(flat) => {
            let n = flat.len() / 2;
            out.elements = Some(n as u64);
            let mut lines = Vec::new();
            for pair in flat.chunks(2).take(limit) {
                let field = String::from_utf8_lossy(&pair[0]);
                let val = pair
                    .get(1)
                    .map(|v| String::from_utf8_lossy(v).into_owned())
                    .unwrap_or_default();
                lines.push(format!("{field} = {val}"));
            }
            out.body = lines.join("\n");
            if n > limit {
                out.body
                    .push_str(&format!("\n… {} more fields (truncated)", n - limit));
            }
        }
        Err(e) => out.error = Some(e.to_string()),
    }
}

async fn zset(conn: &mut ConnectionManager, raw_key: &[u8], out: &mut KeyValue, cap: usize) {
    let limit = coll_cap(cap);
    let items: redis::RedisResult<Vec<(Vec<u8>, f64)>> = redis::cmd("ZRANGE")
        .arg(raw_key)
        .arg(0)
        .arg(limit as i64 - 1)
        .arg("WITHSCORES")
        .query_async(conn)
        .await;
    match items {
        Ok(members) => {
            out.elements = Some(members.len() as u64);
            out.body = members
                .iter()
                .map(|(m, score)| format!("{score}  {}", String::from_utf8_lossy(m)))
                .collect::<Vec<_>>()
                .join("\n");
        }
        Err(e) => out.error = Some(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_with_expiry_formats_each_case() {
        assert_eq!(ttl_with_expiry(Some(-1)), "persistent");
        assert_eq!(ttl_with_expiry(Some(-2)), "-");
        assert_eq!(ttl_with_expiry(None), "-");
        let s = ttl_with_expiry(Some(3600));
        // relative seconds + an absolute date in parentheses
        assert!(s.starts_with("3600s (expires "), "got: {s}");
        assert!(s.ends_with(')'));
        // the date looks like YYYY-MM-DD HH:MM:SS
        assert!(
            s.contains("-") && s.matches(':').count() >= 2,
            "expected a timestamp: {s}"
        );
    }

    #[test]
    fn pattern_for_plain_type() {
        assert_eq!(
            match_pattern_for("WpDcVyo5fP", "product-detail-route"),
            "WpDcVyo5fP:product-detail-route*"
        );
    }

    #[test]
    fn pattern_restores_tag_control_bytes() {
        let p = match_pattern_for("ns", "\\x01tags\\x01product");
        assert_eq!(p, "ns:\u{0001}tags\u{0001}product*");
    }

    #[test]
    fn pattern_for_hash_bucket_drops_marker() {
        assert_eq!(match_pattern_for("ns", "(hash)"), "ns:*");
        assert_eq!(match_pattern_for("ns", "(no-type)"), "ns:*");
    }

    #[test]
    fn escape_unescape_roundtrip() {
        let raw = b"ns:\x01tags\x01product".to_vec();
        let esc = escape_key(&raw);
        assert_eq!(esc, "ns:\\x01tags\\x01product");
        assert_eq!(unescape_key(&esc), raw);
    }

    // ---- Integration against a real Redis (db 15), skipped if unreachable ----

    const TEST_URL: &str = "redis://127.0.0.1:6379/15";

    async fn conn() -> Option<ConnectionManager> {
        let client = redis::Client::open(TEST_URL).ok()?;
        crate::connect(client, 1).await.ok()
    }

    #[tokio::test]
    async fn lists_keys_and_decodes_gzip_string() {
        let Some(mut c) = conn().await else {
            eprintln!("skipping: no Redis");
            return;
        };
        let ns = "swrci-inspect";
        // clean
        let _: () = redis::cmd("DEL")
            .arg(format!("{ns}:cached-page-aaaa"))
            .query_async(&mut c)
            .await
            .unwrap();
        // gzip payload
        let gz = {
            use std::io::Write;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(b"{\"compressed\":true}").unwrap();
            e.finish().unwrap()
        };
        let _: () = redis::cmd("SET")
            .arg(format!("{ns}:cached-page-aaaa"))
            .arg(gz)
            .query_async(&mut c)
            .await
            .unwrap();

        // list
        let list = list_keys(&mut c, ns, "cached-page").await;
        assert!(
            list.keys.iter().any(|k| k.contains("cached-page-aaaa")),
            "key not listed: {:?}",
            list.keys
        );

        // value decoded
        let v = get_value(&mut c, &format!("{ns}:cached-page-aaaa"), 20_000).await;
        assert_eq!(v.redis_type, "string");
        assert_eq!(v.encoding, Some(crate::value::Encoding::Gzip));
        assert!(
            v.body.contains("compressed"),
            "body not decoded: {}",
            v.body
        );

        let _: () = redis::cmd("DEL")
            .arg(format!("{ns}:cached-page-aaaa"))
            .query_async(&mut c)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn decodes_symfony_wrapped_embedded_zlib() {
        // Mirrors the real fAXeCszHqI:system-config payload: binary header +
        // serialized tag envelope + an embedded zlib stream holding the config.
        let Some(mut c) = conn().await else {
            eprintln!("skipping: no Redis");
            return;
        };
        let key = "swrci-inspect:wrapped-config";
        let mut raw = vec![0x9d, 0, 0, 0, 0, 0, 0, 0, 0];
        raw.extend_from_slice(b"a:1:{i:0;s:13:\"system-config\";}s:60:\"");
        let inner = b"a:2:{s:4:\"core\";a:1:{s:5:\"email\";s:9:\"a@shop.de\";}}";
        {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(inner).unwrap();
            raw.extend(e.finish().unwrap());
        }
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(raw)
            .query_async(&mut c)
            .await
            .unwrap();

        let v = get_value(&mut c, key, 20_000).await;
        assert_eq!(v.redis_type, "string");
        assert_eq!(v.encoding, Some(crate::value::Encoding::Zlib));
        assert!(v.stream_offset.unwrap_or(0) > 0, "should be embedded");
        assert!(!v.is_binary, "inner config is text");
        assert!(v.body.contains("email") && v.body.contains("a@shop.de"));

        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn binary_string_has_both_hex_and_lossy_text() {
        let Some(mut c) = conn().await else {
            eprintln!("skipping: no Redis");
            return;
        };
        let key = "swrci-inspect:binblob";
        // readable serialized marker + invalid UTF-8 bytes
        let mut payload = b"ca:2:{i:0;s:5:\"hello\";}".to_vec();
        payload.extend_from_slice(&[0xff, 0xfe, 0x00, 0x9d]);
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(payload)
            .query_async(&mut c)
            .await
            .unwrap();

        let v = get_value(&mut c, key, 20_000).await;
        assert_eq!(v.redis_type, "string");
        assert!(v.is_binary, "should be flagged binary");
        // hex view present and shows the bytes…
        let hex = v.hex.expect("hex view");
        assert!(hex.contains("ff fe 00 9d"));
        // …text view still surfaces the readable serialized structure.
        assert!(v.body.contains("ca:2:") && v.body.contains("hello"));

        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reads_set_members() {
        let Some(mut c) = conn().await else {
            eprintln!("skipping: no Redis");
            return;
        };
        let key = "swrci-inspect:\u{0001}tags\u{0001}thing";
        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut c)
            .await
            .unwrap();
        let _: () = redis::cmd("SADD")
            .arg(key)
            .arg("member-a")
            .arg("member-b")
            .query_async(&mut c)
            .await
            .unwrap();
        // the escaped form is what the UI passes back
        let v = get_value(&mut c, "swrci-inspect:\\x01tags\\x01thing", 20_000).await;
        assert_eq!(v.redis_type, "set");
        assert_eq!(v.elements, Some(2));
        assert!(v.body.contains("member-a") && v.body.contains("member-b"));
        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut c)
            .await
            .unwrap();
    }
}
