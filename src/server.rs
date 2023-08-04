use crate::config::HttpProxy;
use crate::proxy;
use axum::extract::State;
use axum::http;
use axum::http::StatusCode;
use axum::{routing::get, Router};
use hyper;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http;

#[derive(Debug)]
pub struct ServeError {
    config: HttpProxy,
    error: hyper::Error,
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
    // FIXME: handle HTTPS method in HttpProxy field.
    // FIXME: apply client request timeout starting from
    // socket accept, such that the socket is closed completely
    // when the client hangs on there without sending any data.
    // -- otherwise clients can hold HTTP connections open
    // for no reason during extended periods of time.
    // FIXME: wholepage caching to be implemented, only
    // for valid 200 OK responses, as a layer of the method
    // router rather than at the top level app router.
    pub fn new(config: HttpProxy) -> Server {
        Server { config: config }
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
            let bodytimeout = tower_http::timeout::RequestBodyTimeoutLayer::new(
                self.config.client_request_timeout,
            );
            router = router.route(
                path.as_str(),
                get(handle_with_proxy).with_state(state).layer(bodytimeout),
            );
        }

        let timeouter = tower_http::timeout::TimeoutLayer::new(self.config.proxy_response_timeout);
        router = router.layer(timeouter);

        let addr = SocketAddr::new(self.config.host, self.config.port);
        let maybe_bound = axum::Server::try_bind(&addr);
        match maybe_bound {
            Ok(bound) => match bound
                .http1_header_read_timeout(self.config.client_request_timeout)
                .serve(router.into_make_service())
                .await
            {
                Ok(()) => Ok(()),
                Err(error) => Err(ServeError {
                    config: self.config.clone(),
                    error,
                }),
            },
            Err(error) => Err(ServeError {
                config: self.config.clone(),
                error,
            }),
        }
    }
}
