/// Caching primitives used by metrics-proxy.
///
/// This file contains caching primitives for both the code that filters and
/// reduces time resolution of metrics, as well as the code that deals with
/// post-processed HTTP responses from backends.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

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

// FIXME: implement asynchronous cache eviction.
#[derive(Clone)]
/// Multi-threaded, generic cache for future results.
/// During optimistic caching case, it takes less than 1% of CPU
/// to perform the bookkeeping overhead that the cache creates.
pub struct DeadlineCacher<T: Sized> {
    variants: Arc<RwLock<HashMap<String, Arc<RwLock<Option<(Instant, Arc<T>)>>>>>>,
    staleness: Duration,
}

impl<T> DeadlineCacher<T> {
    pub fn new(staleness: Duration) -> Self {
        DeadlineCacher {
            variants: Arc::new(RwLock::new(HashMap::new())),
            staleness: staleness,
        }
    }

    /// Get a cached item based on a cache key, or if not cached,
    /// use a future that returns a tuple (T, cached) indicating
    /// if the instance of T should be cached or not.
    ///
    /// Returns a tuple (Arc<T>, bool) where the boolean indicates
    /// if the result was from cache or not.
    pub async fn get_or_insert_with(
        &self,
        cache_key: String,
        fut: impl Future<Output = (T, bool)>,
    ) -> (Arc<T>, bool) {
        let read_hashmap = self.variants.clone();
        let read_hashmap_locked = read_hashmap.read().await;

        let now = std::time::Instant::now();
        // Check the cache as reader first.
        if let Some(item_lock) = &read_hashmap_locked.get(&cache_key).clone() {
            let val = item_lock.read().await;
            if val.is_some() {
                // Guard has a value.  Let's see if it's fresh.
                let (saved_at, item) = val.clone().unwrap();
                if let Some(when_minus_staleness) = now.checked_sub(self.staleness) {
                    if saved_at > when_minus_staleness {
                        return (item, true);
                    }
                }
            }
        };

        // We did not find it in the cache, or the guard was
        // empty.  Drop the read lock and prepare to grab the
        // write one in order to put the value in the hashmap.
        drop(read_hashmap_locked);
        drop(read_hashmap);

        let write_hashmap = self.variants.clone();
        let mut write_hashmap_locked = write_hashmap.write().await;

        // Write tombstone into hashmap.
        let entry_guard = Arc::new(RwLock::new(None));
        let mut item_locked_for_write = entry_guard.write().await;
        write_hashmap_locked.insert(cache_key.clone(), entry_guard.clone());
        drop(write_hashmap_locked);
        drop(write_hashmap);

        // No cache hit.  Fetch and cache if the fetcher function
        // returns true as part of its return tuple.  Fetching
        // is accomplished by actually running the future, which
        // is otherwise left unrun and dropped if not used.
        let (item3, cache_it) = fut.await;
        let now: Instant = std::time::Instant::now();
        let arced = Arc::new(item3);
        if cache_it == true {
            // Save into cache *only* if cache_it is true.
            // Otherwise leave the empty None guard in place.
            *item_locked_for_write = Some((now, arced.clone()));
        };
        drop(item_locked_for_write);

        (arced, false)
    }
}
