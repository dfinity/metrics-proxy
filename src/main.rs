use clap::Parser;
use tokio;
use tokio::task::JoinSet;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct MetricsProxyArgs {
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() {
    let args = MetricsProxyArgs::parse();
    let maybecfg = metrics_proxy::config::Config::try_from(args.config.clone());
    if let Err(error) = maybecfg {
        eprintln!("Error parsing {}: {}", args.config.display(), error);
        std::process::exit(exitcode::CONFIG);
    }
    let mut set = JoinSet::new();
    let proxylist: Vec<metrics_proxy::config::HttpProxy> = maybecfg.unwrap().into();
    for proxy in proxylist {
        let server = metrics_proxy::server::Server::new(proxy);
        set.spawn(async move { server.serve().await });
    }
    while let Some(res) = set.join_next().await {
        if let Err(error) = res.unwrap() {
            eprintln!("HTTP server failed: {}", error);
            std::process::exit(exitcode::OSERR);
        }
    }
}
