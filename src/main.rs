use axum_otel_metrics::HttpMetricsLayerBuilder;
use clap::Parser;
use tokio::task::JoinSet;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct MetricsProxyArgs {
    config: std::path::PathBuf,
}

pub async fn run() {
    let args = MetricsProxyArgs::parse();
    let maybecfg = metrics_proxy::config::Config::try_from(args.config.clone());
    if let Err(error) = maybecfg {
        eprintln!("Error parsing {}: {}", args.config.display(), error);
        std::process::exit(exitcode::CONFIG);
    }
    let mut set = JoinSet::new();

    let cfg = maybecfg.unwrap();
    let mut telemetry = cfg.metrics.clone().map(|listener| {
        (
            listener,
            HttpMetricsLayerBuilder::new()
                .with_service_name(env!("CARGO_PKG_NAME").to_string())
                .with_service_version(env!("CARGO_PKG_VERSION").to_string())
                .build(),
        )
    });

    let proxylist: Vec<metrics_proxy::config::HttpProxy> = cfg.into();

    for proxy in proxylist {
        let mut server = metrics_proxy::server::Server::from(proxy);
        telemetry = match telemetry {
            Some((t, m)) => {
                server = server.with_telemetry(m.clone());
                Some((t, m))
            }
            _ => None,
        };
        set.spawn(async move { server.serve().await });
    }
    if let Some((t, m)) = telemetry {
        let server = metrics_proxy::server::Server::for_service_metrics(t).with_telemetry(m);
        set.spawn(async move { server.serve().await });
    }

    while let Some(res) = set.join_next().await {
        if let Err(error) = res.unwrap() {
            eprintln!("HTTP server failed: {error}");
            std::process::exit(exitcode::OSERR);
        }
    }
}

#[tokio::main]
async fn main() {
    run().await
}
