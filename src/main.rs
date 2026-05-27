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

    simple_logger::init_with_level(log::Level::Info).unwrap();

    // Multiple TLS-using deps (rustls, hyper-rustls, tokio-rustls, reqwest)
    // may each pull in their own crypto provider feature, so rustls 0.23
    // can't pick one automatically. Install aws_lc_rs explicitly.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cfg = maybecfg.unwrap();
    let metrics = cfg
        .metrics
        .clone()
        .map(|listener| (listener, metrics_proxy::metrics::Metrics::new()));

    let proxylist: Vec<metrics_proxy::config::HttpProxy> = cfg.into();

    for proxy in proxylist {
        let mut server = metrics_proxy::server::Server::from(proxy);
        if let Some((_, m)) = &metrics {
            server = server.with_metrics(m.clone());
        }
        set.spawn(async move { server.serve().await });
    }
    if let Some((listener, m)) = metrics {
        let server = metrics_proxy::server::Server::for_service_metrics(listener).with_metrics(m);
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
