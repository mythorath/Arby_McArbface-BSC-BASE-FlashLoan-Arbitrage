use prometheus::{
    register_counter, register_counter_vec, register_gauge, register_histogram,
    Counter, CounterVec, Gauge, Histogram, Encoder, TextEncoder,
};
use tokio::task::JoinHandle;
use tracing::info;

lazy_static::lazy_static! {
    pub static ref PATHS_EVALUATED: Counter = register_counter!(
        "arb_paths_evaluated_total",
        "Total number of path evaluations"
    ).unwrap();

    pub static ref PROFITABLE_FOUND: Counter = register_counter!(
        "arb_profitable_found_total",
        "Total profitable paths found"
    ).unwrap();

    pub static ref SUBMIT_ATTEMPTS: Counter = register_counter!(
        "arb_submit_attempts_total",
        "Total bundle submission attempts"
    ).unwrap();

    pub static ref SUBMIT_BY_VENUE: CounterVec = register_counter_vec!(
        "arb_submit_by_venue_total",
        "Submission attempts by venue and tier",
        &["venue", "tier"]
    ).unwrap();

    pub static ref SUBMIT_LANDED: CounterVec = register_counter_vec!(
        "arb_submit_landed_total",
        "Tx landing outcomes",
        &["status"]
    ).unwrap();

    pub static ref WARP_SPEND_USD: Counter = register_counter!(
        "arb_warp_spend_usd_total",
        "Total USD spent on Warp/Trader calls at $0.15 each"
    ).unwrap();

    pub static ref SCAN_LATENCY: Histogram = register_histogram!(
        "arb_scan_latency_seconds",
        "Per-block scan latency in seconds",
        vec![0.001, 0.005, 0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0]
    ).unwrap();

    pub static ref STATE_REFRESH_LATENCY: Histogram = register_histogram!(
        "arb_state_refresh_seconds",
        "State refresh latency in seconds",
        vec![0.005, 0.01, 0.02, 0.05, 0.1, 0.25, 0.5]
    ).unwrap();

    pub static ref CURRENT_BLOCK: Gauge = register_gauge!(
        "arb_current_block",
        "Latest block number processed"
    ).unwrap();

    pub static ref POOL_COUNT: Gauge = register_gauge!(
        "arb_pool_count",
        "Number of pools being monitored"
    ).unwrap();

    pub static ref GAS_SPENT_WEI: Counter = register_counter!(
        "arb_gas_spent_wei_total",
        "Total gas spent in wei"
    ).unwrap();

    pub static ref PATH_SUPPRESSED: Counter = register_counter!(
        "arb_path_suppressed_total",
        "Paths suppressed by circuit breaker"
    ).unwrap();

    pub static ref BUILDER_SIM_REJECT: Counter = register_counter!(
        "arb_builder_sim_reject_total",
        "Builder simulation rejections (pre-revert signal)"
    ).unwrap();

    pub static ref BACKRUN_CANDIDATES: Counter = register_counter!(
        "arb_backrun_candidates_total",
        "Pending swaps matched for backrun evaluation"
    ).unwrap();

    pub static ref BACKRUN_SUBMITTED: Counter = register_counter!(
        "arb_backrun_submitted_total",
        "Backrun bundles submitted"
    ).unwrap();
}

async fn metrics_handler() -> String {
    let encoder = TextEncoder::new();
    let mut buf = Vec::new();
    encoder.encode(&prometheus::gather(), &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

pub fn start_metrics_server(port: u16) -> JoinHandle<()> {
    tokio::spawn(async move {
        let app = axum::Router::new()
            .route("/metrics", axum::routing::get(metrics_handler))
            .route("/health", axum::routing::get(|| async { "ok" }));

        let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(port, error = %e, "Failed to bind metrics port, metrics HTTP disabled");
                loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; }
            }
        };
        info!(port, "Prometheus /metrics HTTP server listening");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "Metrics server failed");
        }
    })
}
