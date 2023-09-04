use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

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
