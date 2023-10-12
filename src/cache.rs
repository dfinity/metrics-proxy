/// Caching primitives used by metrics-proxy.
///
/// This file contains caching primitives for both the code that filters and
/// reduces time resolution of metrics, as well as the code that deals with
/// post-processed HTTP responses from backends.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

use itertools::Itertools;
use prometheus_parse::Sample;

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
#[derive(Clone)]
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
