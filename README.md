# Prometheus metrics proxy

This program is a metrics proxy that allows proxying of metrics served by
standard Prometheus exporters, with the ability to select which exporters
will be proxied, which metrics will appear in the results, and with which
granularity these metrics will be updated.

## Configuration

The program takes a single configuration file path as its sole mandatory
argument, and the configuration file follows this format:

```
proxies:
  - listen_on:
      address: 0.0.0.0:18080
      handler: /metrics
    connect_to:
      method: http
      address: localhost:9100
      handler: /metrics
    label_filters:
      - regex: .*
        actions:
          - drop
      - regex: node_cpu_.*
        actions:
          - cache:
              duration: 5s
          - keep
```

Another sample configuration file has been included with the source
at [`example-config.yaml`](./example-config.yaml).

The meaning of each structure in the configuration file is explained below.

### `proxies`

A top-level list of dictionaries, each of which must contain a `proxy` object.

### `proxy`

A dictionary that contains three key / value pairs:

* `listen_on`
* `connect_to`
* `label_filters`

### `listen_on`

A dictionary of `address` (host / port) and `handler` (HTTP path of the
proxy, which must be rooted at `/`).  The address and handler together
are used to produce the final result URL on which this proxy will answer.

Optionally, the `method` key can be `http` (no HTTPS support yet).

No two `listen_on` entries may share the same address, handler and method,
since then the proxy would not be able to decide which one of the targets
should be proxied.

Additionally, two timeouts can be specified (as a Rust duration string):

* `header_read_timeout` (default 5 seconds) specifies how long the
  proxy will wait for clients to finish uploading their request body.
  Specifically, the time counts from the first HTTP-level byte the
  client sends to the last carriage return in the request header.
* `request_response_timeout` (default 5 seconds more than `timeout` in
  the `connect_to` structure) specifies how long the whole request may
  take (including the time spent contacting the proxy) all the way until
  the last byte is sent to the client.

### `connect_to`

A dictionary of `address` (host / port), `handler` (HTTP path of the target
exporter, which must be rooted at `/`) and method (`http` or `https`, default
`http`).  The methd, address and handler together are used to produce the
final result URL to which this proxy will connect to.

Optionally, a `timeout` can be specified (as a Rust duration string) to
instruct the proxy on how long it should wait until the proxied exporter has
fully responded.  The default timeout is 30 seconds.

### `label_filters`

A list of one or more `label_filter`.  Each `label_filter` is applied
in sequence to each metric as it comes through the proxy pipeline.

### `label_filter`

A dictionary, patterned similarly to Prometheus metric relabeling, as
specified by the following keys:

* `regex` is a Rust-compatible regular expression (anchored at beginning and
  end) which will be used to match against the concatenated `source_labels`.
* `source_labels` is an optional list of metrics label names that will be
  concatenated using `separator` to match against `regex`.  It defaults to
  `__name__` which in standard Prometheus means the primary metric name.
* `separator` is optional and defaults to a semicolon, as it does in standard
  Prometheus metric relabeling configuration.
* `actions` is a list of `action` to be taken on every metric whose label
  concatenation matches the regex.

### `action`

Currently, there are three action classes:

* `keep`: this action instructs the proxy to keep a matching metric,
  useful when a prior `drop` instructed the proxy to drop it, and
  this decision needs to be overridden / refined.
* `drop`: this action instructs the proxy to drop a matching metric,
  and the metric will be dropped unless subsequent actions insist
  it should be `keep`ed.
* `cache`: this action (with a mandatory `duration` parameter)
  instructs the proxy to serve the metric from a cache unless the
  cache entry is older than the specified duration.
