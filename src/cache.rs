use http::Request;
use http_body;
use http_body::Body;

use futures_util::FutureExt;
use hyper::Response;
use itertools::Itertools;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tower::{Layer, Service};

use crate::metrics::CacheMetrics;
use axum::http;
use hyper::body::Bytes;
use opentelemetry::KeyValue;
use prometheus_parse::{self, Sample};

/// Caching primitives used by metrics-proxy.
///
/// This file contains caching primitives for both the code that filters and
/// reduces time resolution of metrics, as well as the code that deals with
/// post-processed HTTP responses from backends.

#[derive(Debug, PartialEq, Eq, Hash)]
struct LabelPair {
    name: String,
    value: String,
}
#[derive(Debug, PartialEq, Eq, Hash)]
struct OrderedLabelSet(Vec<LabelPair>);

/// A comparable struct used to retrieve values from a cache keyed by label names.
impl From<&Sample> for OrderedLabelSet {
    fn from(x: &Sample) -> OrderedLabelSet {
        // We use mut here because the alternative (concat)
        // requires LabelPair to be clonable (less efficient).
        let mut labelset: Vec<LabelPair> = vec![LabelPair {
            name: "__name__".to_string(),
            value: x.metric.to_string(),
        }];
        labelset.extend(
            x.labels
                .iter()
                .map(|m| LabelPair {
                    name: m.0.to_string(),
                    value: m.1.to_string(),
                })
                .collect::<Vec<LabelPair>>(),
        );
        OrderedLabelSet(
            labelset
                .into_iter()
                .sorted_unstable_by_key(|k| k.name.to_string())
                .collect(),
        )
    }
}

struct SampleCacheEntry {
    sample: prometheus_parse::Sample,
    saved_at: Instant,
}

#[derive(Default)]
pub struct SampleCacheStore {
    cache: HashMap<OrderedLabelSet, SampleCacheEntry>,
}

impl SampleCacheStore {
    #[must_use]
    pub fn get(
        &self,
        sample: &prometheus_parse::Sample,
        when: Instant,
        staleness: Duration,
    ) -> Option<Sample> {
        let key = OrderedLabelSet::from(sample);
        let value = self.cache.get(&key);
        match value {
            Some(v) => {
                if let Some(when_minus_staleness) = when.checked_sub(staleness) {
                    if v.saved_at > when_minus_staleness {
                        Some(v.sample.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn put(&mut self, sample: prometheus_parse::Sample, at_: Instant) {
        let cache = &mut self.cache;
        cache.insert(
            OrderedLabelSet::from(&sample),
            SampleCacheEntry {
                sample,
                saved_at: at_,
            },
        );
    }
}

// FIXME: make this into a Tower service.
#[derive(Debug, Clone)]
/// Multi-threaded, generic cache for future results.
/// During optimistic caching case, it takes less than 1% of CPU
/// to perform the bookkeeping overhead that the cache creates.
pub struct DeadlineCacher<Y: Sized + 'static> {
    variants: Arc<Mutex<HashMap<String, Arc<RwLock<Option<Arc<Y>>>>>>>,
    staleness: Duration,
}

impl<Y: Sync + Send> DeadlineCacher<Y> {
    pub fn new(staleness: Duration) -> Self {
        DeadlineCacher {
            variants: Arc::new(Mutex::new(HashMap::new())),
            staleness: staleness,
        }
    }

    /// Get a cached item based on a cache key, or if not cached,
    /// use a future that returns a tuple (T, cached) indicating
    /// if the instance of T should be cached or not.
    ///
    /// Returns a tuple (Arc<Y>, bool) where the boolean indicates
    /// if the result was from cache or not.
    pub async fn get_or_insert_with(
        &self,
        cache_key: String,
        fut: impl Future<Output = (Y, bool)>,
    ) -> (Arc<Y>, bool) {
        let hashmap = self.variants.clone();
        let mut locked_hashmap = hashmap.lock().await;

        // Check the cache as reader first.
        if let Some(item_lock) = locked_hashmap.get(&cache_key).clone() {
            let guard = item_lock.read().await;
            // The following item will always be Some() for pages that were
            // cacheable in the past.  If the page wasn't cacheable at the
            // last request, this will be None and we will proceed below.
            // The entry guard exists solely so that other requests not hitting
            // the same cache key can proceed in parallel without full contention
            // on the cache hashmap itself.
            if let Some(cached_value) = guard.clone() {
                // Cache has an entry guard, and entry guard has a value.
                return (cached_value, true);
            }
        }

        // We did not find it in the cache (it's not cached)
        // Write the entry guard into the cache, then.
        // lock the entry guard and unlock the cache.
        let entry_guard = Arc::new(RwLock::new(None));
        let mut locked_entry_guard = entry_guard.write().await;
        locked_hashmap.insert(cache_key.clone(), entry_guard.clone());
        drop(locked_hashmap);

        // Fetch and cache if the fetcher function returns true
        // as part of its return tuple.  Fetching is accomplished
        // by actually running the future, which is otherwise left
        // unrun and dropped (therefore canceled) if not used.
        let (item3, cache_it) = fut.await;
        let arced = Arc::new(item3);
        if cache_it == true {
            // Save into cache *only* if cache_it is true.
            // Otherwise leave the empty None guard in place.
            *locked_entry_guard = Some(arced.clone());
        };

        // Now, schedule the asynchronous removal of the cached
        // item from the hashmap.
        let staleness = self.staleness.clone();
        let variants = self.variants.clone();
        tokio::task::spawn(async move {
            tokio::time::sleep(staleness).await;
            let mut write_hashmap = variants.lock().await;
            write_hashmap.remove(&cache_key);
            drop(write_hashmap);
        });

        // Now unlock the entry guard altogether.  Other threads
        // trying to access the same cache key can proceed.
        drop(locked_entry_guard);

        (arced, false)
    }
}

#[derive(Clone)]
pub struct CacheLayer {
    cacher: DeadlineCacher<CachedResponse>,
}

impl CacheLayer {
    pub fn new(staleness: Duration) -> Self {
        CacheLayer {
            cacher: DeadlineCacher::new(staleness),
        }
    }
}

impl<S> Layer<S> for CacheLayer {
    type Service = CacheService<S>;

    fn layer(&self, service: S) -> Self::Service {
        CacheService {
            cacher: self.cacher.clone(),
            metrics: CacheMetrics::new(),
            inner: service,
        }
    }
}

#[derive(Debug, Clone)]
struct CachedResponse {
    version: http::Version,
    status: http::StatusCode,
    headers: http::HeaderMap,
    contents: Bytes,
}

#[derive(Clone)]
pub struct CacheService<S> {
    cacher: DeadlineCacher<CachedResponse>,
    metrics: CacheMetrics,
    inner: S,
}

// https://docs.rs/tower/latest/tower/trait.Service.html#server
// https://docs.rs/tower/latest/tower/trait.Service.html#backpressure
impl<S> Service<Request<axum::body::Body>> for CacheService<S>
where
    S: Service<Request<axum::body::Body>, Response = Response<axum::body::BoxBody>>
        + std::marker::Send
        + 'static,
    S::Error: Into<Box<dyn std::error::Error>>,
    S::Error: std::fmt::Debug,
    S::Error: std::marker::Send,
    <S as Service<http::Request<hyper::Body>>>::Future: std::marker::Send,
{
    type Error = S::Error;
    type Response = Response<axum::body::BoxBody>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<axum::body::Body>) -> Self::Future {
        let reqversion = request.version();
        let reqheaders = request.headers();
        let frontend_label = format!(
            "{}{}",
            match reqheaders.get("host") {
                Some(host) => match host.to_str() {
                    Ok(hoststr) => hoststr,
                    Err(_) => "invalid-hostname",
                },
                None => "no-hostname",
            },
            request.uri()
        );
        let cache_key = format!(
            "{}\n{:?}\n{:?}",
            request.uri(),
            reqheaders.get("Authorization"),
            reqheaders.get("Proxy-Authorization")
        );
        let client_call = self.inner.call(request);
        let cacher = self.cacher.clone();
        let metrics = self.metrics.clone();

        let fut = async move {
            fn badresp(version: http::Version, reason: String) -> (CachedResponse, bool) {
                (
                    CachedResponse {
                        version: version,
                        status: http::StatusCode::INTERNAL_SERVER_ERROR,
                        headers: http::HeaderMap::new(),
                        contents: reason.into(),
                    },
                    false,
                )
            }
            match client_call.await {
                Err(e) => badresp(
                    reqversion,
                    format!("Proxy error downstream from cacher: {:?}", e).to_string(),
                ),
                Ok(res) => {
                    let (parts, body) = res.into_parts();
                    match hyper::body::to_bytes(body).await {
                        Err(e) => badresp(
                            parts.version,
                            format!("Proxy error fetching body: {:?}", e).to_string(),
                        ),
                        Ok(data) => (
                            CachedResponse {
                                version: parts.version,
                                status: parts.status,
                                headers: parts.headers,
                                contents: data,
                            },
                            parts.status.is_success(),
                        ),
                    }
                }
            }
        };

        async move {
            let (res, cached) = cacher.get_or_insert_with(cache_key, fut).await;

            // Note the caching status of the returned page.
            match cached {
                true => metrics.http_cache_hits,
                false => metrics.http_cache_misses,
            }
            .add(
                1,
                &[
                    KeyValue::new("http_response_status_code", res.status.as_str().to_string()),
                    KeyValue::new("frontend", frontend_label),
                ],
            );

            // Formulate a response based on the returned page.
            let mut respb = http::response::Response::builder().version(res.version);
            let headers = respb.headers_mut().unwrap();
            headers.extend(res.headers.clone());
            let resp = respb
                .status(res.status)
                .body(axum::body::BoxBody::new(
                    axum::body::Full::new(res.contents.clone()).map_err(axum::Error::new),
                ))
                .unwrap();
            Ok(resp)
        }
        .boxed()
    }
}
