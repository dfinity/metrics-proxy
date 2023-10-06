use opentelemetry::global;
use opentelemetry::metrics::Counter;

#[derive(Clone)]
pub struct CustomMetrics {
    pub http_cache_misses: Counter<u64>,
    pub http_cache_hits: Counter<u64>,
}

impl CustomMetrics {
    pub fn new() -> Self {
        let meter = global::meter("axum-app");
        // Create two instruments.
        let http_cache_misses = meter
            .u64_counter("http.cache.misses")
            .with_description("Total number of HTTP cache misses")
            .init();
        let http_cache_hits = meter
            .u64_counter("http.cache.hits")
            .with_description("Total number of HTTP cache hits")
            .init();
        CustomMetrics {
            http_cache_misses,
            http_cache_hits,
        }
    }
}
