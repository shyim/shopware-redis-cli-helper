//! The `get` command: fetch one key and print its decoded value.
//!
//! This is the non-interactive twin of the TUI value inspector — same type-aware
//! reading and automatic gzip/zlib/zstd decompression (including streams embedded
//! after a Symfony cache wrapper), but printed to stdout so it can be piped.

use crate::connect;
use crate::inspect::{self, KeyValue};
use crate::value::Encoding;
use anyhow::Result;
use clap::Args;
use humansize::{format_size, DECIMAL};

/// No truncation for `get` by default — the user asked for the value, give it
/// all. (The TUI uses a small display cap; this is effectively unbounded.)
const GET_CAP: usize = usize::MAX / 2;

#[derive(Args, Debug)]
pub struct GetArgs {
    /// The key to read. The `\x01tags\x01…` escaped form is accepted for tag
    /// sets (the same notation the insights views show).
    pub key: String,

    /// Print the value as a hex dump instead of decoded text.
    #[arg(long)]
    pub hex: bool,

    /// Print only the value body (no type/size/TTL header). Default when stdout
    /// is piped; use --header to force the header.
    #[arg(long)]
    pub raw: bool,

    /// Always print the metadata header, even when piped.
    #[arg(long, conflicts_with = "raw")]
    pub header: bool,

    /// Emit a JSON object with the value and its metadata.
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: GetArgs, client: redis::Client, connect_timeout: u64) -> Result<()> {
    let mut conn = connect(client, connect_timeout).await?;
    let v = inspect::get_value(&mut conn, &args.key, GET_CAP).await;

    if let Some(err) = &v.error {
        anyhow::bail!("{err}");
    }

    if args.json {
        print_json(&v);
        return Ok(());
    }

    // Header: shown unless --raw, or unless piped (so plain pipelines get just
    // the value). --header overrides the piped default.
    let show_header =
        args.header || (!args.raw && std::io::IsTerminal::is_terminal(&std::io::stdout()));
    if show_header {
        eprintln!("{}", header_line(&v));
    }

    if args.hex {
        match &v.hex {
            Some(h) => print!("{h}"),
            None => eprintln!("(no hex view for type {})", v.redis_type),
        }
    } else {
        println!("{}", v.body);
    }
    Ok(())
}

fn header_line(v: &KeyValue) -> String {
    let mut parts = vec![format!("type={}", v.redis_type)];
    if let Some(b) = v.size_bytes {
        parts.push(format!("size={}", format_size(b, DECIMAL)));
    }
    if let Some(n) = v.elements {
        parts.push(format!("elements={n}"));
    }
    parts.push(format!("ttl={}", inspect::ttl_with_expiry(v.ttl)));
    if let Some(enc) = v.encoding {
        if enc != Encoding::Plain {
            match v.stream_offset {
                Some(off) if off > 0 => {
                    parts.push(format!("decompressed={} (embedded @ {off})", enc.label()))
                }
                _ => parts.push(format!("decompressed={}", enc.label())),
            }
        }
    }
    if v.is_binary {
        parts.push("binary".into());
    }
    format!("# {}", parts.join("  "))
}

/// Absolute expiry timestamp (RFC3339) for a positive TTL, else None.
fn expires_at(ttl: Option<i64>) -> Option<String> {
    match ttl {
        Some(secs) if secs >= 0 => {
            let when = chrono::Local::now() + chrono::Duration::seconds(secs);
            Some(when.to_rfc3339())
        }
        _ => None,
    }
}

fn print_json(v: &KeyValue) {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        key: &'a str,
        #[serde(rename = "type")]
        redis_type: &'a str,
        ttl: Option<i64>,
        /// Absolute expiry as RFC3339, when the key has a positive TTL.
        expires_at: Option<String>,
        size_bytes: Option<u64>,
        elements: Option<u64>,
        encoding: Option<&'a str>,
        embedded_offset: Option<usize>,
        is_binary: bool,
        value: &'a str,
    }
    let out = Out {
        key: &v.key,
        redis_type: &v.redis_type,
        ttl: v.ttl,
        expires_at: expires_at(v.ttl),
        size_bytes: v.size_bytes,
        elements: v.elements,
        encoding: v.encoding.map(|e| e.label()),
        embedded_offset: v.stream_offset.filter(|&o| o > 0),
        is_binary: v.is_binary,
        value: &v.body,
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
