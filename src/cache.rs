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

impl OrderedLabelSet {
    fn new(x: &Sample) -> OrderedLabelSet {
        let mut labelset: Vec<LabelPair> = vec![];
        labelset.push(LabelPair {
            name: "__name__".to_string(),
            value: x.metric.to_string(),
        });
        for (k, v) in x.labels.iter() {
            labelset.push(LabelPair {
                name: k.to_string(),
                value: v.to_string(),
            });
        }
        let res: Vec<LabelPair> = labelset
            .into_iter()
            .sorted_unstable_by_key(|k| k.name.to_string())
            .collect();
        OrderedLabelSet(res)
    }
}

struct SampleCacheEntry {
    sample: prometheus_parse::Sample,
    saved_at: Instant,
}

pub struct SampleCache {
    cache: HashMap<OrderedLabelSet, SampleCacheEntry>,
}

impl SampleCache {
    pub fn new() -> Self {
        SampleCache {
            cache: HashMap::new(),
        }
    }
    pub fn get(
        &self,
        sample: &prometheus_parse::Sample,
        at_: Instant,
        staleness: Duration,
    ) -> Option<Sample> {
        let key = OrderedLabelSet::new(sample);
        let value = self.cache.get(&key);
        match value {
            Some(v) => {
                if v.saved_at > at_ - staleness {
                    return Some(v.sample.clone());
                }
            }
            _ => {
                return None;
            }
        }
        None
    }

    pub fn put(&mut self, sample: prometheus_parse::Sample, at_: Instant) {
        let cache = &mut self.cache;
        cache.insert(
            OrderedLabelSet::new(&sample),
            SampleCacheEntry {
                sample,
                saved_at: at_,
            },
        );
    }
}
