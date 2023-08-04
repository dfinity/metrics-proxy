use tokio;
use tokio::task::JoinSet;

#[tokio::main]
async fn main() {
    let f = "config.yaml".to_string();
    let maybecfg = metrics_proxy::config::load_config(f.clone());
    if let Err(error) = maybecfg {
        println!("Error parsing {}: {}", f, error);
        return;
    }
    let cfg = maybecfg.unwrap();
    let proxylist = metrics_proxy::config::convert_config_to_proxy_list(cfg);
    //println!("{:#?}", proxylist);

    let mut set = JoinSet::new();
    for proxy in proxylist {
        let server = metrics_proxy::server::Server::new(proxy);
        set.spawn(async move { server.serve().await });
    }
    while let Some(res) = set.join_next().await {
        if let Err(error) = res.unwrap() {
            panic!("Fatal HTTP server failure: {}", error);
        }
    }
}
