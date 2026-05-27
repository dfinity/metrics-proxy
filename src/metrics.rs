use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::extract::MatchedPath;
use axum::http::{Request, Response};
use http_body::Body;
use pin_project_lite::pin_project;
use prometheus::{
    process_collector::ProcessCollector, Encoder, HistogramOpts, HistogramVec, IntCounterVec,
    IntGaugeVec, Opts, Registry, TextEncoder,
};
use tower::{Layer, Service};

/// Default duration buckets in seconds, covering ~5ms to ~10s.
const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Default size buckets in bytes, covering ~100B to ~10MB.
const SIZE_BUCKETS: &[f64] = &[
    100.0, 1_000.0, 10_000.0, 100_000.0, 1_000_000.0, 10_000_000.0,
];

#[derive(Clone)]
pub struct Metrics {
    pub registry: Arc<Registry>,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
    request_size: HistogramVec,
    response_size: HistogramVec,
    active_requests: IntGaugeVec,
    pub cache_hits: IntCounterVec,
    pub cache_misses: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        // ProcessCollector::for_self() can fail on platforms without /proc.
        let _ = registry.register(Box::new(ProcessCollector::for_self()));

        let request_labels = &[
            "http_request_method",
            "http_route",
            "http_response_status_code",
            "server_address",
        ];
        let active_labels = &["http_request_method", "url_scheme"];

        let requests_total = IntCounterVec::new(
            Opts::new(
                "requests_total",
                "How many HTTP requests processed, partitioned by status code and HTTP method.",
            ),
            request_labels,
        )
        .unwrap();
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "http_server_request_duration_seconds",
                "The HTTP request latencies in seconds.",
            )
            .buckets(DURATION_BUCKETS.to_vec()),
            request_labels,
        )
        .unwrap();
        let request_size = HistogramVec::new(
            HistogramOpts::new(
                "http_server_request_size_bytes",
                "The HTTP request sizes in bytes.",
            )
            .buckets(SIZE_BUCKETS.to_vec()),
            request_labels,
        )
        .unwrap();
        let response_size = HistogramVec::new(
            HistogramOpts::new(
                "http_server_response_size_bytes",
                "The HTTP response sizes in bytes.",
            )
            .buckets(SIZE_BUCKETS.to_vec()),
            request_labels,
        )
        .unwrap();
        let active_requests = IntGaugeVec::new(
            Opts::new(
                "http_server_active_requests",
                "The number of active HTTP requests.",
            ),
            active_labels,
        )
        .unwrap();

        let cache_labels = &["http_response_status_code", "frontend"];
        let cache_hits = IntCounterVec::new(
            Opts::new("http_cache_hits_total", "Total number of HTTP cache hits"),
            cache_labels,
        )
        .unwrap();
        let cache_misses = IntCounterVec::new(
            Opts::new(
                "http_cache_misses_total",
                "Total number of HTTP cache misses",
            ),
            cache_labels,
        )
        .unwrap();

        for c in [&requests_total, &cache_hits, &cache_misses] {
            registry.register(Box::new(c.clone())).unwrap();
        }
        for h in [&request_duration, &request_size, &response_size] {
            registry.register(Box::new(h.clone())).unwrap();
        }
        registry.register(Box::new(active_requests.clone())).unwrap();

        Metrics {
            registry: Arc::new(registry),
            requests_total,
            request_duration,
            request_size,
            response_size,
            active_requests,
            cache_hits,
            cache_misses,
        }
    }

    /// Encode the registry in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut buf = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut buf)
            .ok();
        String::from_utf8(buf).unwrap_or_default()
    }

    pub fn layer(&self) -> MetricsLayer {
        MetricsLayer {
            metrics: self.clone(),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct MetricsLayer {
    metrics: Metrics,
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    metrics: Metrics,
}

impl<S, ReqB, RespB> Service<Request<ReqB>> for MetricsService<S>
where
    S: Service<Request<ReqB>, Response = Response<RespB>>,
    ReqB: Body,
    RespB: Body,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = MetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqB>) -> Self::Future {
        let method = req.method().as_str().to_string();
        let url_scheme = req.uri().scheme_str().unwrap_or("http").to_string();
        let path = req
            .extensions()
            .get::<MatchedPath>()
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let host = req
            .headers()
            .get(axum::http::header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let req_size = approximate_request_size(&req) as f64;

        self.metrics
            .active_requests
            .with_label_values(&[&method, &url_scheme])
            .inc();

        MetricsFuture {
            inner: self.inner.call(req),
            start: Instant::now(),
            method,
            url_scheme,
            path,
            host,
            req_size,
            metrics: self.metrics.clone(),
        }
    }
}

pin_project! {
    pub struct MetricsFuture<F> {
        #[pin]
        inner: F,
        start: Instant,
        method: String,
        url_scheme: String,
        path: String,
        host: String,
        req_size: f64,
        metrics: Metrics,
    }
}

impl<F, B, E> Future for MetricsFuture<F>
where
    F: Future<Output = Result<Response<B>, E>>,
    B: Body,
{
    type Output = Result<Response<B>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let response = match this.inner.poll(cx) {
            Poll::Ready(r) => r?,
            Poll::Pending => return Poll::Pending,
        };

        this.metrics
            .active_requests
            .with_label_values(&[this.method, this.url_scheme])
            .dec();

        let latency = this.start.elapsed().as_secs_f64();
        let status = response.status().as_u16().to_string();
        let labels = [
            this.method.as_str(),
            this.path.as_str(),
            status.as_str(),
            this.host.as_str(),
        ];
        let res_size = response.body().size_hint().upper().unwrap_or(0) as f64;

        this.metrics.requests_total.with_label_values(&labels).inc();
        this.metrics
            .request_duration
            .with_label_values(&labels)
            .observe(latency);
        this.metrics
            .request_size
            .with_label_values(&labels)
            .observe(*this.req_size);
        this.metrics
            .response_size
            .with_label_values(&labels)
            .observe(res_size);

        Poll::Ready(Ok(response))
    }
}

/// Approximate the wire size of an incoming request from method, path,
/// header bytes, and Content-Length. Matches the heuristic used by the
/// previous middleware (originally from echo-contrib's prometheus middleware).
fn approximate_request_size<B>(req: &Request<B>) -> usize {
    let mut s = req.uri().path().len() + req.method().as_str().len();
    s += req.uri().host().map(|h| h.len()).unwrap_or(0);
    for (k, v) in req.headers() {
        s += k.as_str().len() + v.as_bytes().len();
    }
    s += req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    s
}
