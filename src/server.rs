use crate::config::{self, HttpProxy};
use crate::proxy;
use axum::extract::State;
use axum::http;
use axum::http::StatusCode;
use axum::middleware::map_response;
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
                ServeErrorKind::HyperError(e) => format!("{e}"),
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

pub struct Server {
    config: HttpProxy,
}

impl From<HttpProxy> for Server {
    fn from(config: HttpProxy) -> Self {
        Server { config }
    }
}

impl Server {
    // FIXME: wholepage caching to be implemented, only
    // for valid 200 OK responses, as a layer of the method
    // router rather than at the top level app router.
    // This caching layer should not apply to Prometheus metrics serving.
    // May want to use sister of map_response called map_request
    // https://docs.rs/axum/latest/axum/middleware/fn.map_response.html

    /// Starts an HTTP or HTTPS server on the configured host and port,
    /// proxying requests to each one of the targets defined in the
    /// `handlers` of the `HttpProxy` config.
    ///
    /// # Errors
    /// * `StartError` is returned if the server fails to start.
    pub async fn serve(self) -> Result<(), StartError> {
        // Short helper to issue backend request.
        async fn handle_with_proxy(
            State(proxy): State<Arc<proxy::MetricsProxier>>,
            headers: http::HeaderMap,
        ) -> (StatusCode, http::HeaderMap, std::string::String) {
            proxy.handle(headers).await
        }

        // Short helper to map 408 from request response timeout layer to 504.
        async fn gateway_timeout<B>(
            mut response: axum::response::Response<B>,
        ) -> axum::response::Response<B> {
            if response.status() == http::StatusCode::REQUEST_TIMEOUT {
                *response.status_mut() = http::StatusCode::GATEWAY_TIMEOUT;
            }
            response
        }

        let listener = self.config.listen_on;

        let mut router: Router<_, _> = Router::new();
        let bodytimeout =
            tower_http::timeout::RequestBodyTimeoutLayer::new(listener.header_read_timeout);

        for (path, target) in self.config.handlers.clone() {
            let state = Arc::new(proxy::MetricsProxier::from(target));
            router = router.route(
                path.as_str(),
                get(handle_with_proxy)
                    .with_state(state)
                    .layer(tower::ServiceBuilder::new().layer(bodytimeout.clone())),
            );
        }

        // Last the timeout layer.
        // The timeout layer returns HTTP status code 408 if the backend
        // fails to respond on time.  When this happens, we map that code
        // to 503 Gateway Timeout.
        // (Contrast with backend down -- this usually requires a response
        // of 502 Bad Gateway, which is already issued by the client handler.)
        let timeout_handling_layer =
            tower_http::timeout::TimeoutLayer::new(listener.request_response_timeout);
        router = router
            .layer(timeout_handling_layer)
            .layer(map_response(gateway_timeout));

        let incoming = AddrIncoming::bind(&listener.sockaddr).map_err(|error| StartError {
            addr: listener.sockaddr,
            error: ServeErrorKind::HyperError(error),
        })?;

        match &listener.protocol {
            config::Protocol::Http => {
                hyper::Server::builder(incoming)
                    .http1_header_read_timeout(listener.header_read_timeout)
                    .serve(router.into_make_service())
                    .await
            }
            config::Protocol::Https { certificate, key } => {
                hyper::Server::builder(
                    TlsAcceptor::builder()
                        .with_single_cert(certificate.clone(), key.clone())
                        .map_err(|error| StartError {
                            addr: listener.sockaddr,
                            error: ServeErrorKind::RustlsError(error),
                        })?
                        .with_all_versions_alpn()
                        .with_incoming(incoming),
                )
                .http1_header_read_timeout(listener.header_read_timeout)
                .serve(router.into_make_service())
                .await
            }
        }
        .map_err(|error| StartError {
            addr: listener.sockaddr,
            error: ServeErrorKind::HyperError(error),
        })
    }
}
