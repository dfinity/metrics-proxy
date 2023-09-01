use crate::config::HttpProxyTarget;
use crate::{cache::SampleCache, client, config};
use axum::http;
use axum::http::StatusCode;
use itertools::Itertools;
use prometheus_parse::{self, Sample};
use reqwest::header;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::f64;
use std::iter::zip;
use std::sync::{Arc, Mutex};
use std::time::Duration;

static HOPBYHOP: [&str; 8] = [
    "keep-alive",
    "transfer-encoding",
    "te",
    "connection",
    "trailer",
    "upgrade",
    "proxy-authorization",
    "proxy-authenticate",
];
static STRIP_FROM_RESPONSE: [&str; 1] = ["content-length"];

static PROXIED_CLIENT_HEADERS: [&str; 1] = ["accept"];

fn safely_clone_response_headers(orgheaders: header::HeaderMap) -> http::HeaderMap {
    // println!("Original: {:?}", orgheaders);
    let mut headers = http::HeaderMap::new();
    for (k, v) in orgheaders.into_iter() {
        if let Some(kk) = k {
            let lower = kk.to_string().to_lowercase();
            if !HOPBYHOP.contains(&lower.as_str()) && !STRIP_FROM_RESPONSE.contains(&lower.as_str())
            {
                headers.insert(kk, v);
            }
        }
    }
    // println!("Filtered: {:?}", headers);
    headers
}

fn safely_clone_request_headers(orgheaders: http::HeaderMap) -> header::HeaderMap {
    // println!("Original: {:?}", orgheaders);
    let mut headers = header::HeaderMap::new();
    for (k, v) in orgheaders.into_iter() {
        if let Some(kk) = k {
            if PROXIED_CLIENT_HEADERS.contains(&kk.to_string().to_lowercase().as_str()) {
                headers.insert(kk, v);
            }
        }
    }
    // println!("Filtered: {:?}", headers);
    headers
}

fn fallback_headers() -> header::HeaderMap {
    let mut fallback_headers = http::HeaderMap::new();
    fallback_headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    fallback_headers
}

fn render_scrape_data(scrape: prometheus_parse::Scrape) -> String {
    fn render_sample(sample: &prometheus_parse::Sample) -> Vec<String> {
        fn render_labels(labels: &prometheus_parse::Labels, extra: Option<String>) -> String {
            let mut joined = labels
                .iter()
                .map(|(n, v)| format!("{}=\"{}\"", n, v))
                .collect::<Vec<String>>();

            joined.sort();
            if let Some(o) = extra {
                joined.push(o);
            };

            if joined.is_empty() {
                "".to_string()
            } else {
                "{".to_string() + &joined.join(",") + "}"
            }
        }

        let values = match &sample.value {
            prometheus_parse::Value::Untyped(val) => vec![format!("{:e}", val)],
            prometheus_parse::Value::Counter(val) => vec![format!("{:e}", val)],
            prometheus_parse::Value::Gauge(val) => vec![format!("{:e}", val)],
            prometheus_parse::Value::Histogram(val) => val
                .iter()
                .map(|h| format!("{:e}", h.count))
                .collect::<Vec<String>>(),
            prometheus_parse::Value::Summary(val) => val
                .iter()
                .map(|h| format!("{:e}", h.count))
                .collect::<Vec<String>>(),
        };
        let labels = match &sample.value {
            prometheus_parse::Value::Untyped(_val) => vec![None],
            prometheus_parse::Value::Counter(_val) => vec![None],
            prometheus_parse::Value::Gauge(_val) => vec![None],
            prometheus_parse::Value::Histogram(val) => val
                .iter()
                .map(|h| {
                    Some(format!("le=\"{}\"", {
                        if h.less_than == f64::INFINITY {
                            "+Inf".to_string()
                        } else if h.less_than == f64::NEG_INFINITY {
                            "-Inf".to_string()
                        } else {
                            format!("{}", h.less_than)
                        }
                    }))
                })
                .collect::<Vec<Option<String>>>(),
            prometheus_parse::Value::Summary(val) => val
                .iter()
                .map(|h| Some(format!("quantile=\"{}\"", h.quantile)))
                .collect::<Vec<Option<String>>>(),
        };
        
        zip(values, labels)
            .map(|(value, extra_label)| {
                format!(
                    "{}{} {}",
                    sample.metric,
                    render_labels(&sample.labels, extra_label),
                    value
                )
            })
            .collect::<Vec<String>>()
    }

    fn render_response(scrape: prometheus_parse::Scrape) -> String {
        let mut help = scrape.docs.clone();
        let rendered = scrape
            .samples
            .iter()
            .sorted_by(|sample1, sample2| {
                if sample1.metric > sample2.metric {
                    Ordering::Greater
                } else if sample1.metric < sample2.metric {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .map(|sample| {
                (
                    &sample.metric,
                    &sample.value,
                    render_sample(sample).join("\n"),
                )
            })
            .map(|(metric, value, rendered)| {
                if let Some(h) = help.remove(metric) {
                    format!(
                        "# HELP {} {}\n# TYPE {} {}\n{}",
                        metric,
                        h,
                        metric,
                        match value {
                            prometheus_parse::Value::Untyped(_) => "untyped",
                            prometheus_parse::Value::Counter(_) => "counter",
                            prometheus_parse::Value::Gauge(_) => "gauge",
                            prometheus_parse::Value::Histogram(_) => "histogram",
                            prometheus_parse::Value::Summary(_) => "summary",
                        },
                        rendered
                    )
                } else {
                    rendered
                }
            })
            .collect::<Vec<String>>()
            .join("\n")
            + "\n";
        rendered
    }

    render_response(scrape)
}

#[derive(Clone)]
pub struct ProxyAdapter {
    target: HttpProxyTarget,
    cache: Arc<Mutex<SampleCache>>, // FIXME cache
}

impl ProxyAdapter {
    pub fn new(target: HttpProxyTarget) -> Self {
        ProxyAdapter {
            target,
            cache: Arc::new(Mutex::new(SampleCache::new())), // FIXME cache
        }
    }

    pub async fn handle(&self, headers: http::HeaderMap) -> (StatusCode, http::HeaderMap, String) {
        let clientheaders = safely_clone_request_headers(headers);
        let result = client::scrape(&self.target.connect_to, clientheaders).await;
        match result {
            Err(error) => match error {
                client::ScrapeError::Non200(non200) => (
                    non200.status,
                    safely_clone_response_headers(non200.headers),
                    non200.data,
                ),
                client::ScrapeError::ParseError(parseerror) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    fallback_headers(),
                    format!("Error parsing output.\n\n{:#?}", parseerror),
                ),
                client::ScrapeError::FetchError(fetcherror) => {
                    let mut statuscode = StatusCode::BAD_GATEWAY;
                    let mut errmsg = format!("The target is down.\n\n{:#?}", fetcherror);
                    if fetcherror.is_timeout() {
                        // 504 target timed out
                        statuscode = StatusCode::GATEWAY_TIMEOUT;
                        errmsg = format!("The target is timing out.\n\n{:#?}", fetcherror);
                    }
                    (statuscode, fallback_headers(), errmsg)
                }
            },
            Ok(parsed) => (
                StatusCode::OK,
                safely_clone_response_headers(parsed.headers),
                render_scrape_data(self.apply_filters(parsed.series)),
            ),
        }
    }

    // FIXME: perhaps we need a completely separate module just for filtering.
    fn apply_filters(&self, series: prometheus_parse::Scrape) -> prometheus_parse::Scrape {
        let selectors = &self.target.label_filters;
        let mut samples: Vec<prometheus_parse::Sample> = vec![];
        let mut docs: HashMap<String, String> = HashMap::new();

        fn label_value(
            metric: &String,
            labels: &prometheus_parse::Labels,
            label_name: &String,
        ) -> String {
            if label_name == "__name__" {
                metric.to_string()
            } else if labels.contains_key(label_name.as_str()) {
                labels.get(label_name.as_str()).unwrap().to_string()
            } else {
                // No label with that name.  No match.
                // This is consistent with how Prometheus metric relabeling
                // deals with absent labels.
                "".to_string()
            }
        }

        {
            let mut cache = self.cache.lock().unwrap();

            for sample in series.samples.into_iter() {
                let mut keep: Option<bool> = None;
                let now = std::time::Instant::now();
                let mut cached_sample: Option<Sample> = None;
                // The following value, if true at the end of this loop,
                // indicates whether the sample should be cached for
                // future lookups.  Values are only cached when the
                // cache is consulted and the result is a cache miss.
                let mut cache_sample = false;

                for selector in selectors.iter() {
                    let source_labels = &selector.source_labels;
                    let label_values = source_labels
                        .iter()
                        .map(|label_name| label_value(&sample.metric, &sample.labels, label_name))
                        .collect::<Vec<String>>()
                        .join(selector.separator.as_str());
                    for action in &selector.actions {
                        if selector.regex.is_match(&label_values) {
                            match action {
                                config::ConfigLabelFilterAction::Keep => {
                                    keep = Some(true);
                                }
                                config::ConfigLabelFilterAction::Drop => {
                                    keep = Some(false);
                                }
                                config::ConfigLabelFilterAction::Cache { duration } => {
                                    // If the cache has not expired according to the duration,
                                    // then the cache returns the cached sample.
                                    // Else, if the cache has expired according to the duration,
                                    // then the cache returns nothing.
                                    // Below, we insert it into the cache if nothing was returned
                                    // into the cache at all.
                                    let staleness: Duration = duration.to_owned().into();
                                    cached_sample = cache.get(&sample, now, staleness);
                                    cache_sample = true;
                                }
                            }
                        }
                    }
                }

                // Ignore this sample if the conclusion is that we were going to drop it anyway.
                if let Some(trulykeep) = keep {
                    if !trulykeep {
                        continue;
                    }
                }

                // Add this sample's metric name documentation if not yet added.
                if !docs.contains_key(&sample.metric) && series.docs.contains_key(&sample.metric) {
                    docs.insert(
                        sample.metric.to_owned(),
                        series.docs.get(&sample.metric).unwrap().clone(),
                    );
                }

                match cached_sample {
                    Some(s) => samples.push(s),
                    None => {
                        if cache_sample {
                            cache.put(sample.clone(), now);
                        }
                        samples.push(sample)
                    }
                }
            }
        }

        prometheus_parse::Scrape {
            samples,
            docs,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigConnectTo, ConfigLabelFilter, HttpProxyTarget, Protocol};
    use dissimilar::diff as __diff;
    use duration_string::DurationString;
    use std::time::Duration;

    use super::render_scrape_data;

    pub fn format_diff(chunks: Vec<dissimilar::Chunk>) -> String {
        let mut buf = String::new();
        for chunk in chunks {
            let formatted = match chunk {
                dissimilar::Chunk::Equal(text) => text.into(),
                dissimilar::Chunk::Delete(text) => format!("\x1b[41m{}\x1b[0m", text),
                dissimilar::Chunk::Insert(text) => format!("\x1b[42m{}\x1b[0m", text),
            };
            buf.push_str(&formatted);
        }
        buf
    }

    macro_rules! assert_eq_text {
        ($left:expr, $right:expr) => {
            assert_eq_text!($left, $right,)
        };
        ($left:expr, $right:expr, $($tt:tt)*) => {{
            let left = $left;
            let right = $right;
            if left != right {
                if left.trim() == right.trim() {
                    std::eprintln!("Left:\n{:?}\n\nRight:\n{:?}\n\nWhitespace difference\n", left, right);
                } else {
                    let diff = __diff(left, right);
                    std::eprintln!("Left:\n{}\n\nRight:\n{}\n\nDiff:\n{}\n", left, right, format_diff(diff));
                }
                std::eprintln!($($tt)*);
                panic!("text differs");
            }
        }};
    }

    fn make_test_proxy_target(filters: Vec<ConfigLabelFilter>) -> HttpProxyTarget {
        HttpProxyTarget {
            connect_to: ConfigConnectTo {
                protocol: Protocol::Http,
                address: "localhost:8080".to_string(),
                handler: "/metrics".to_string(),
                timeout: DurationString::new(Duration::new(5, 0)),
            },
            label_filters: filters,
        }
    }

    fn make_adapter_filter_tester(filters: Vec<ConfigLabelFilter>) -> crate::proxy::ProxyAdapter {
        let target = make_test_proxy_target(filters);
        return crate::proxy::ProxyAdapter::new(target);
    }

    struct TestPayload {
        sorted_text: String,
        parsed_scrape: prometheus_parse::Scrape,
    }

    impl TestPayload {
        fn from_scrape(scrape: prometheus_parse::Scrape) -> Self {
            let rendered = render_scrape_data(scrape.clone());
            let mut sorted_rendered: Vec<String> = rendered.lines().map(|s| s.to_owned()).collect();
            sorted_rendered.sort();
            let sorted_text = sorted_rendered.join("\n");
            return TestPayload {
                sorted_text: sorted_text,
                parsed_scrape: scrape,
            };
        }

        fn from_text(text: &str) -> Self {
            let parsed_scrape =
                prometheus_parse::Scrape::parse(text.lines().map(|s| Ok(s.to_owned()))).unwrap();
            return TestPayload::from_scrape(parsed_scrape);
        }
    }

    #[test]
    fn test_proxy_no_filtering() {
        let adapter = make_adapter_filter_tester(vec![]);
        let text = r#"
# HELP node_softnet_times_squeezed_total Number of times processing packets ran out of quota
# TYPE node_softnet_times_squeezed_total counter
node_softnet_times_squeezed_total{cpu="0"} 0
node_softnet_times_squeezed_total{cpu="1"} 0
node_softnet_times_squeezed_total{cpu="10"} 0
node_softnet_times_squeezed_total{cpu="11"} 0
node_softnet_times_squeezed_total{cpu="12"} 0
node_softnet_times_squeezed_total{cpu="13"} 0
node_softnet_times_squeezed_total{cpu="14"} 0
node_softnet_times_squeezed_total{cpu="15"} 0
node_softnet_times_squeezed_total{cpu="2"} 0
node_softnet_times_squeezed_total{cpu="3"} 0
node_softnet_times_squeezed_total{cpu="4"} 0
node_softnet_times_squeezed_total{cpu="5"} 0
node_softnet_times_squeezed_total{cpu="6"} 0
node_softnet_times_squeezed_total{cpu="7"} 0
node_softnet_times_squeezed_total{cpu="8"} 0
node_softnet_times_squeezed_total{cpu="9"} 0
"#;
        let inp_ = TestPayload::from_text(text);
        let exp_ = TestPayload::from_text(text);
        let filtered = adapter.apply_filters(inp_.parsed_scrape);
        let out_ = TestPayload::from_scrape(filtered);
        assert_eq_text!(exp_.sorted_text.as_str(), out_.sorted_text.as_str());
    }

    #[test]
    fn test_proxy_one_label_filtering() {
        let adapter = make_adapter_filter_tester(
            serde_yaml::from_str(
                r#"
- regex: node_softnet_times_squeezed_total
  actions: [drop]
- source_labels: [cpu]
  regex: "1"
  actions: [keep]
"#,
            )
            .unwrap(),
        );
        let inp_ = TestPayload::from_text(
            r#"
# HELP node_softnet_times_squeezed_total Number of times processing packets ran out of quota
# TYPE node_softnet_times_squeezed_total counter
node_softnet_times_squeezed_total{cpu="0"} 0
node_softnet_times_squeezed_total{cpu="1"} 0
node_softnet_times_squeezed_total{cpu="10"} 0
node_softnet_times_squeezed_total{cpu="11"} 0
node_softnet_times_squeezed_total{cpu="12"} 0
node_softnet_times_squeezed_total{cpu="13"} 0
node_softnet_times_squeezed_total{cpu="14"} 0
node_softnet_times_squeezed_total{cpu="15"} 0
node_softnet_times_squeezed_total{cpu="2"} 0
node_softnet_times_squeezed_total{cpu="3"} 0
node_softnet_times_squeezed_total{cpu="4"} 0
node_softnet_times_squeezed_total{cpu="5"} 0
node_softnet_times_squeezed_total{cpu="6"} 0
node_softnet_times_squeezed_total{cpu="7"} 0
node_softnet_times_squeezed_total{cpu="8"} 0
node_softnet_times_squeezed_total{cpu="9"} 0
"#,
        );
        let exp_ = TestPayload::from_text(
            r#"
# HELP node_softnet_times_squeezed_total Number of times processing packets ran out of quota
# TYPE node_softnet_times_squeezed_total counter
node_softnet_times_squeezed_total{cpu="1"} 0
"#,
        );
        let filtered = adapter.apply_filters(inp_.parsed_scrape);
        let out_ = TestPayload::from_scrape(filtered);
        assert_eq_text!(exp_.sorted_text.as_str(), out_.sorted_text.as_str());
    }

    #[test]
    fn test_caching() {
        let adapter = make_adapter_filter_tester(
            serde_yaml::from_str(
                r#"
- regex: node_frobnicated
  actions:
  - cache:
      duration: 10ms
"#,
            )
            .unwrap(),
        );

        // First scrape.  Metric should be there, and
        // will not be filtered.  Input should be same as output.
        let first_input = TestPayload::from_text(
            r#"
# HELP node_frobnicated Number of times processing packets ran out of quota
# TYPE node_frobnicated counter
node_frobnicated{cpu="0"} 0
"#,
        );
        let first_filtered = adapter.apply_filters(first_input.parsed_scrape);
        let first_output = TestPayload::from_scrape(first_filtered);
        assert_eq_text!(
            first_input.sorted_text.as_str(),
            first_output.sorted_text.as_str()
        );

        // Now we run a different metric value thru the filter.
        // The filter should have given us the same value since 10ms have not passed.
        // In other words, the output of this one should be
        // the same as the input of the prior filter run.
        let second_input = TestPayload::from_text(
            r#"
# HELP node_frobnicated Number of times processing packets ran out of quota
# TYPE node_frobnicated counter
node_frobnicated{cpu="0"} 25
"#,
        );
        let second_output =
            TestPayload::from_scrape(adapter.apply_filters(second_input.parsed_scrape.clone()));
        assert_eq_text!(
            first_input.sorted_text.as_str(),
            second_output.sorted_text.as_str()
        );

        std::thread::sleep(Duration::from_millis(10));

        // Now we run the same input as in the prior step, but because
        // time has passed, then the filter will let the updated value pass.
        // In other words, the output of this filter round should be the
        // input of the prior (-> the second) round.
        let third_output =
            TestPayload::from_scrape(adapter.apply_filters(second_input.parsed_scrape.clone()));
        assert_eq_text!(
            second_input.sorted_text.as_str(),
            third_output.sorted_text.as_str()
        );
    }
}
