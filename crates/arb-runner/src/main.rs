mod config;
mod metrics;
mod pricing;
mod runner;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .with_thread_ids(true)
        .init();

    info!("arbv2 runner starting");

    let args: Vec<String> = std::env::args().collect();
    let config_path = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("config/bsc.toml");

    let smoke_test = args.iter().any(|a| a == "--smoke-test");
    let force_fire = args.iter().any(|a| a == "--force-fire");

    if smoke_test {
        info!("SMOKE TEST MODE — will fire one path, wait for receipt, then exit");
    }
    if force_fire {
        info!("FORCE FIRE MODE — will fire best candidate regardless of gate, wait for receipt, then exit");
    }

    let cfg = config::load_config(config_path)?;
    info!(chain = %cfg.chain.name, config = config_path, "Configuration loaded");

    let metrics_port: u16 = std::env::var("METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9090);
    let _metrics_handle = metrics::start_metrics_server(metrics_port);

    runner::run(cfg, smoke_test || force_fire).await?;

    Ok(())
}
