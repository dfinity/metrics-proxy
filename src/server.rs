use crate::config::{self, HttpProxy};
use crate::proxy;
use axum::extract::State;
use axum::http;
use axum::http::StatusCode;
use axum::{routing::get, Router};
use hyper;
use hyper::server::conn::AddrIncoming;
use hyper_rustls::TlsAcceptor;
use rustls;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http;

#[derive(Debug)]
pub enum ServeErrorKind {
    HyperError(hyper::Error),
    RustlsError(rustls::Error),
}

impl fmt::Display for ServeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                ServeErrorKind::HyperError(e) => format!("{}", e),
                ServeErrorKind::RustlsError(ef) => format!("{}", ef),
            }
        )
    }
}

#[derive(Debug)]
pub struct ServeError {
    config: HttpProxy,
    error: ServeErrorKind,
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "cannot listen on host {} port {}: {}",
            self.config.host, self.config.port, self.error
        )
    }
}
pub struct Server {
    config: HttpProxy,
}

impl Server {
    // FIXME: wholepage caching to be implemented, only
    // for valid 200 OK responses, as a layer of the method
    // router rather than at the top level app router.
    pub fn new(config: HttpProxy) -> Server {
        Server { config }
    }

    pub async fn serve(&self) -> Result<(), ServeError> {
        async fn handle_with_proxy(
            State(proxy): State<Arc<proxy::ProxyAdapter>>,
            headers: http::HeaderMap,
        ) -> (StatusCode, http::HeaderMap, std::string::String) {
            proxy.handle(headers).await
        }

        let mut router: Router<_, _> = Router::new();

        for (path, target) in self.config.handlers.clone().into_iter() {
            let state = Arc::new(proxy::ProxyAdapter::new(target));
            let bodytimeout =
                tower_http::timeout::RequestBodyTimeoutLayer::new(self.config.header_read_timeout);
            router = router.route(
                path.as_str(),
                get(handle_with_proxy).with_state(state).layer(bodytimeout),
            );
        }

        let timeouter =
            tower_http::timeout::TimeoutLayer::new(self.config.request_response_timeout);
        router = router.layer(timeouter);

        let addr = SocketAddr::new(self.config.host, self.config.port);
        let incoming = AddrIncoming::bind(&addr).map_err(|error| ServeError {
            config: self.config.clone(),
            error: ServeErrorKind::HyperError(error),
        })?;

        match self.config.protocol {
            config::Protocol::Http => {
                hyper::Server::builder(incoming)
                    .http1_header_read_timeout(self.config.header_read_timeout)
                    .serve(router.into_make_service())
                    .await
            }
            config::Protocol::Https => {
                hyper::Server::builder(
                    TlsAcceptor::builder()
                        .with_single_cert(
                            self.config.certificate.clone().unwrap(),
                            self.config.key.clone().unwrap(),
                        )
                        .map_err(|error| ServeError {
                            config: self.config.clone(),
                            error: ServeErrorKind::RustlsError(error),
                        })?
                        .with_all_versions_alpn()
                        .with_incoming(incoming),
                )
                .http1_header_read_timeout(self.config.header_read_timeout)
                .serve(router.into_make_service())
                .await
            }
        }
        .map_err(|error| ServeError {
            config: self.config.clone(),
            error: ServeErrorKind::HyperError(error),
        })
    }
}
