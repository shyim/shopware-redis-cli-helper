mod cleanup;
mod get;
mod grouping;
mod insights;
mod inspect;
mod report;
mod scanner;
mod stats;
mod tui;
mod value;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use std::time::Duration;

/// Inspect and clean up a Shopware/Symfony Redis cache.
#[derive(Parser, Debug)]
#[command(name = "shopware-redis-cli-helper", version, about)]
struct Cli {
    /// Redis connection URL, e.g. redis://127.0.0.1:6379/0
    #[arg(
        long,
        env = "REDIS_URL",
        default_value = "redis://127.0.0.1:6379",
        global = true
    )]
    url: String,

    /// Seconds to wait for the initial connection (and each request) before
    /// giving up. Prevents hanging on a wrong host/port.
    #[arg(long, default_value_t = 5, global = true)]
    connect_timeout: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Scan the DB and explore namespace/key statistics.
    Insights {
        #[command(subcommand)]
        cmd: Option<insights::InsightsCmd>,
    },
    /// Remove orphaned Symfony cache tags (dry-run by default).
    Cleanup(cleanup::CleanupArgs),
    /// Fetch one key and print its decoded value (auto-decompresses).
    Get(get::GetArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let client = redis::Client::open(cli.url.as_str())
        .map_err(|e| anyhow!("invalid redis url {}: {e}", cli.url))?;

    match cli.command {
        Command::Insights { cmd } => insights::run(cmd, client, cli.connect_timeout).await,
        Command::Cleanup(args) => cleanup::run(args, client, cli.connect_timeout).await,
        Command::Get(args) => get::run(args, client, cli.connect_timeout).await,
    }
}

/// Connect with a bounded timeout so a wrong host/port fails fast instead of
/// hanging on the OS TCP timeout. `ConnectionManager` otherwise retries six
/// times with exponential backoff and no per-attempt timeout — fine for a
/// long-lived service, but here we want a quick, clear failure at startup.
pub async fn connect(client: redis::Client, timeout_secs: u64) -> Result<ConnectionManager> {
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(timeout)
        .set_response_timeout(timeout)
        .set_number_of_retries(1);

    // Outer guard: covers slow paths the per-attempt timeout may not (e.g. DNS
    // resolution, or the one retry's backoff). Worst case ~2× the timeout.
    let overall = timeout.saturating_mul(2);
    match tokio::time::timeout(overall, ConnectionManager::new_with_config(client, config)).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => Err(anyhow!("could not connect to redis: {e}")),
        Err(_) => Err(anyhow!(
            "connection timed out after {}s — check the host/port in --url",
            overall.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::time::Instant;

    #[test]
    fn parses_insights_subcommands() {
        // bare insights -> None subcommand
        let cli = Cli::try_parse_from(["bin", "insights"]).unwrap();
        match cli.command {
            Command::Insights { cmd: None } => {}
            _ => panic!("expected bare insights"),
        }
        // insights tui
        let cli = Cli::try_parse_from(["bin", "insights", "tui"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Insights {
                cmd: Some(insights::InsightsCmd::Tui(_))
            }
        ));
        // insights report --format json
        let cli = Cli::try_parse_from(["bin", "insights", "report", "--format", "json"]).unwrap();
        match cli.command {
            Command::Insights {
                cmd: Some(insights::InsightsCmd::Report(r)),
            } => assert_eq!(r.format, insights::Format::Json),
            _ => panic!("expected insights report json"),
        }
    }

    #[test]
    fn global_url_works_on_any_subcommand() {
        // --url is global, so it can appear before OR after the subcommand.
        let cli =
            Cli::try_parse_from(["bin", "--url", "redis://x:1", "cleanup", "--apply"]).unwrap();
        assert_eq!(cli.url, "redis://x:1");
        assert!(matches!(cli.command, Command::Cleanup(_)));

        let cli =
            Cli::try_parse_from(["bin", "cleanup", "--url", "redis://y:2", "--apply"]).unwrap();
        assert_eq!(cli.url, "redis://y:2");
    }

    #[test]
    fn cleanup_defaults_to_dry_run() {
        let cli = Cli::try_parse_from(["bin", "cleanup"]).unwrap();
        match cli.command {
            Command::Cleanup(a) => assert!(!a.apply, "cleanup must default to dry-run"),
            _ => panic!("expected cleanup"),
        }
    }

    #[test]
    fn get_parses_key_and_flags() {
        let cli = Cli::try_parse_from(["bin", "get", "ns:some-key"]).unwrap();
        match cli.command {
            Command::Get(a) => {
                assert_eq!(a.key, "ns:some-key");
                assert!(!a.hex && !a.json);
            }
            _ => panic!("expected get"),
        }
        let cli = Cli::try_parse_from(["bin", "get", "k", "--hex", "--json"]).unwrap();
        match cli.command {
            Command::Get(a) => assert!(a.hex && a.json),
            _ => panic!("expected get"),
        }
        // --raw and --header are mutually exclusive.
        assert!(Cli::try_parse_from(["bin", "get", "k", "--raw", "--header"]).is_err());
    }

    #[test]
    fn tui_accepts_mode_flag() {
        let cli = Cli::try_parse_from(["bin", "insights", "tui", "--mode", "advanced"]).unwrap();
        match cli.command {
            Command::Insights {
                cmd: Some(insights::InsightsCmd::Tui(a)),
            } => assert_eq!(a.mode, Some(insights::Mode::Advanced)),
            _ => panic!("expected insights tui"),
        }
        // Default: no mode -> None (the TUI will prompt).
        let cli = Cli::try_parse_from(["bin", "insights", "tui"]).unwrap();
        match cli.command {
            Command::Insights {
                cmd: Some(insights::InsightsCmd::Tui(a)),
            } => assert_eq!(a.mode, None),
            _ => panic!("expected insights tui"),
        }
    }

    #[tokio::test]
    async fn connect_fails_fast_on_refused_port() {
        // Port 1 on localhost: refused immediately, well under the timeout.
        let client = redis::Client::open("redis://127.0.0.1:1").unwrap();
        let start = Instant::now();
        let res = connect(client, 5).await;
        assert!(res.is_err(), "expected connection error");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "refused connection should fail near-instantly, took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn connect_times_out_on_blackholed_ip() {
        // 10.255.255.1 is non-routable and silently drops packets, so connect()
        // must give up via the timeout rather than blocking on the OS TCP wait.
        let client = redis::Client::open("redis://10.255.255.1:6379").unwrap();
        let start = Instant::now();
        let res = connect(client, 2).await;
        // Overall guard is 2× the per-attempt timeout; allow generous slack.
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "should time out promptly, took {:?}",
            start.elapsed()
        );
        let msg = match res {
            Ok(_) => panic!("expected a timeout error"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("timed out") || msg.contains("could not connect"),
            "unexpected error message: {msg}"
        );
    }
}
