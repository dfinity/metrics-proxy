use crate::config::{HttpProxy, ListenerSpec, Protocol};
use crate::metrics::Metrics;
use crate::proxy;
use crate::tokiotimer::TokioTimer;
use axum::debug_handler;
use axum::extract::Request;
use axum::extract::State;
use axum::http;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{routing::get, Router};
use hyper;
use hyper::body::Bytes;
use hyper::body::Incoming;
use hyper_util::rt::TokioExecutor;
use hyper_util::server::conn::auto;
use log::{debug, error};
use rustls;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::{rustls::ServerConfig, TlsAcceptor};
use tower::Service;
use tower_http;

/// Logs an error from `serve_connection_with_upgrades`. Routine connection-idle
/// timeouts (clients that connect but never finish sending a request — e.g. LB
/// health-probes or idle keep-alives that time out) are demoted to debug to
/// keep them out of the error stream.
fn log_serve_error(
    kind: &str,
    addr: &SocketAddr,
    err: &(dyn std::error::Error + 'static),
) {
    if let Some(hyper_err) = err.downcast_ref::<hyper::Error>() {
        if hyper_err.is_timeout() {
            debug!(
                "Idle {} connection from {} timed out: {:?}",
                kind, addr, hyper_err
            );
            return;
        }
    }
    error!("Error serving {} request from {}: {:?}", kind, addr, err);
}

#[derive(Debug)]
pub enum ServeErrorKind {
    ListenError(std::io::Error),
    RustlsError(rustls::Error),
}

impl fmt::Display for ServeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                ServeErrorKind::ListenError(e) => format!("{e}"),
                ServeErrorKind::RustlsError(ef) => format!("{ef}"),
            }
        )
    }
}

#[derive(Debug)]
pub struct StartError {
    addr: SocketAddr,
    error: ServeErrorKind,
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "cannot listen on {}: {}", self.addr, self.error)
    }
}

enum ServerKind {
    PrometheusMetricsProxy(HttpProxy),
    PrometheusMetricsServer(ListenerSpec),
}

pub struct Server {
    config: ServerKind,
    metrics: Option<Metrics>,
}

impl From<HttpProxy> for Server {
    fn from(config: HttpProxy) -> Self {
        Self::for_metrics_proxy(config)
    }
}

impl Server {
    #[must_use]
    /// Configures this `Server` to proxy one or more handlers
    /// to a backend endpoint each.
    pub fn for_metrics_proxy(config: HttpProxy) -> Self {
        Server {
            config: ServerKind::PrometheusMetricsProxy(config),
            metrics: None,
        }
    }

    #[must_use]
    /// Configures this `Server` to serve Prometheus metrics
    /// collected during proxying.
    pub fn for_service_metrics(listen_on: ListenerSpec) -> Self {
        Server {
            config: ServerKind::PrometheusMetricsServer(listen_on),
            metrics: None,
        }
    }

    #[must_use]
    /// Enables telemetry collection by wiring up the HTTP request middleware
    /// (for proxy servers) or by exposing the `/metrics` endpoint (for the
    /// service-metrics server).
    pub fn with_metrics(self, metrics: Metrics) -> Self {
        Server {
            config: self.config,
            metrics: Some(metrics),
        }
    }

    /// Starts an HTTP or HTTPS server on the configured host and port,
    /// proxying requests to each one of the targets defined in the
    /// `handlers` of the `HttpProxy` config.
    ///
    /// # Errors
    /// * `StartError` is returned if the server fails to start.
    pub async fn serve(self) -> Result<(), StartError> {
        // Short helper to issue backend request.
        #[debug_handler]
        async fn handle_with_proxy(
            headers: HeaderMap,
            State(proxy): State<proxy::MetricsProxier>,
        ) -> (StatusCode, HeaderMap, Bytes) {
            proxy.handle(headers).await
        }

        // Renders the Prometheus exposition format on the metrics endpoint.
        async fn metrics_handler(State(metrics): State<Metrics>) -> impl IntoResponse {
            (
                [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                metrics.render(),
            )
        }

        let listener = match &self.config {
            ServerKind::PrometheusMetricsProxy(config) => config.listen_on.clone(),
            ServerKind::PrometheusMetricsServer(listen_on) => listen_on.clone(),
        };
        let is_proxy_server = matches!(self.config, ServerKind::PrometheusMetricsProxy(_));

        let mut router: Router<_> = Router::new();
        let bodytimeout =
            tower_http::timeout::RequestBodyTimeoutLayer::new(listener.header_read_timeout);

        router = match self.config {
            ServerKind::PrometheusMetricsProxy(config) => {
                for (path, target) in config.handlers.clone() {
                    let cache_duration = target.clone().cache_duration;
                    let state = proxy::MetricsProxier::from(target);
                    let mut method_router = get(handle_with_proxy)
                        .with_state(state)
                        .layer(tower::ServiceBuilder::new().layer(bodytimeout.clone()));
                    if Duration::from(cache_duration) > Duration::new(0, 0) {
                        method_router = method_router.layer(crate::cache::CacheLayer::new(
                            cache_duration.into(),
                            self.metrics.clone(),
                        ));
                    }
                    router = router.route(path.as_str(), method_router);
                }
                router
            }
            ServerKind::PrometheusMetricsServer(listen_on) => match self.metrics.clone() {
                Some(metrics) => router.route(
                    listen_on.handler.as_str(),
                    get(metrics_handler).with_state(metrics),
                ),
                None => router,
            },
        };

        // Second-to-last the timeout layer.
        // The timeout layer returns 504 Gateway Timeout if the backend fails
        // to respond on time. (Contrast with backend down -- this usually
        // requires a response of 502 Bad Gateway, which is already issued by
        // the client handler.)
        router = router.layer(tower_http::timeout::TimeoutLayer::with_status_code(
            http::StatusCode::GATEWAY_TIMEOUT,
            listener.request_response_timeout,
        ));

        // Then, finally, the telemetry layer.
        // Experimentally, if the telemetry layer does not go last, then
        // whatever errors the timeout layers bubble up, the telemetry
        // layer cannot register as an HTTP error.
        // Only the proxy server records request metrics; the metrics server
        // exposes them, but does not record its own scrape requests.
        if is_proxy_server {
            if let Some(metrics) = &self.metrics {
                router = router.layer(metrics.layer());
            }
        }

        let incoming = TcpListener::bind(&listener.sockaddr)
            .await
            .map_err(|error| StartError {
                addr: listener.sockaddr,
                error: ServeErrorKind::ListenError(error),
            })?;

        let acceptor = match &listener.protocol {
            Protocol::Http => None,
            Protocol::Https { certificate, key } => {
                let mut server_config = ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(certificate.clone(), key.clone_key())
                    .map_err(|error| StartError {
                        addr: listener.sockaddr,
                        error: ServeErrorKind::RustlsError(error),
                    })?;
                server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
                Some(TlsAcceptor::from(Arc::new(server_config)))
            }
        };

        loop {
            match incoming.accept().await {
                Err(err) => {
                    error!("Error accepting new connection: {:?}", err);
                }
                Ok((socket, addr)) => {
                    debug!("Accepted connection from {}", addr);
                    let tower_service = router.clone();
                    let hyper_service =
                        hyper::service::service_fn(move |request: Request<Incoming>| {
                            tower_service.clone().clone().call(request)
                        });
                    let mut builder = auto::Builder::new(TokioExecutor::new());
                    builder
                        .http1()
                        .timer(TokioTimer)
                        .header_read_timeout(listener.header_read_timeout);
                    let tls = acceptor.clone();
                    tokio::task::spawn(async move {
                        match tls {
                            None => {
                                let io = hyper_util::rt::tokio::TokioIo::new(socket);
                                debug!("About to call server to serve {}", addr);
                                let ret = builder
                                    .serve_connection_with_upgrades(io, hyper_service)
                                    .await;
                                if let Err(err) = ret {
                                    log_serve_error("plain", &addr, err.as_ref());
                                }
                            }
                            Some(tls) => {
                                debug!("About to handshake TLS with {}", addr);
                                let ret = tls.accept(socket).await;
                                if let Err(err) = ret {
                                    debug!("Error during TLS handshake from {}: {:?}", addr, err);
                                    return;
                                }
                                let io = hyper_util::rt::tokio::TokioIo::new(ret.unwrap());
                                debug!("About to call TLS-enabled server to serve {}", addr);
                                let ret2 = builder
                                    .serve_connection_with_upgrades(io, hyper_service)
                                    .await;
                                if let Err(err) = ret2 {
                                    log_serve_error("TLS", &addr, err.as_ref());
                                }
                            }
                        }
                    });
                }
            };
        }
    }
}
