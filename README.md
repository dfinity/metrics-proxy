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
      url: http://0.0.0.0:18080
    connect_to:
      url: http://localhost:9100/metrics
    label_filters:
      - regex: .*
        actions:
          - drop
      - regex: node_cpu_.*
        actions:
          - reduce_time_resolution:
              resolution: 5s
          - keep
metrics:
  url: http://127.0.0.1:8008/metrics
```

Another sample configuration file has been included with the source
at [`example-config.yaml`](./example-config.yaml).

The meaning of each structure in the configuration file is explained below.

### `proxies`

A top-level list of dictionaries, each of which must contain a `proxy` object.
Multiple proxies are natively supported.

### `proxy`

A dictionary that contains three key / value pairs:

* `listen_on`: a `listener_spec`
* `connect_to`: a `connector_spec`
* `label_filters`: a `label_filters_spec`

The proxy object determines where to listen on, where to fetch metrics from,
and how the fetched metrics will be post-processed by the proxy.

Optionally, a `cache_duration` can be specified (as a Rust duration string),
which will cause successful metrics fetches from the backend (`connect_to`)
to be cached for that duration of time.  Leaving the value absent will
defeat the cache.

### `listener_spec`

A dictionary that requires only one key: `url`.  Fragments and query
strings in the URL are not supported, and the only schemes supported
are `http` and `https`.  The URL represents the address to which
this specific proxy server will respond to.  Both IP addresses and
host names are supported, but any host name that does not resolve to
an address the system is listening on will later cause an error while
attempting to listen to that address.  The port in the URL is required.

If HTTPS is enabled in the URL, then options `key_file` and
`certificate_file` must be paths pointing to a valid X.509 key file and
certificate file respectively.  Test certificates can be generated with
command
`openssl req -newkey rsa:2048 -nodes -keyout key.pem -x509 -days 365 -out certificate.pem`,
taking care to add a common name to the certificate when prompted.

Additionally, two timeouts can be specified (as a Rust duration string):

* `header_read_timeout` (default 5 seconds) specifies how long the
  proxy will wait for clients to finish uploading their request body.
  Specifically, the time counts from the first HTTP-level byte the
  client sends to the last carriage return in the request header.
* `request_response_timeout` (default 5 seconds more than `timeout` in
  the `connector_spec` structure) specifies how long the whole request may
  take (including the time spent contacting the proxy) all the way until
  the last byte is sent to the client.

No two `listener_spec` entries may share the same host, port, handler path
and protocol, since then the proxy would not be able to decide which one of
the targets should be proxied.

### `connector_spec`

A dictionary with one mandatory field: `url`.  The protocol of the URL
must be one of `http` or `https`, fragments are not allowed in the
URL, and authentication specification is not allowed.

Optionally, a `timeout` can be specified (as a Rust duration string) to
instruct the proxy on how long it should wait until the proxied exporter has
fully responded.  The default timeout is 30 seconds.

### `label_filters_spec`

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
* `reduce_time_resolution`: this action (with a mandatory `resolution`
  parameter as a duration in string form) instructs the proxy to serve
  the metric from a cache unless the cache entry is older than the
  specified time resolution.

### `metrics`

A single `listener_spec` that determines if and where the metrics handler
listens to (the handler dedicated to serving metrics intrinsic to this
program, rather than proxying metrics from other exporters).

If absent, no intrinsic metrics will be made available.

## Operations

### Metrics

Metrics are a vital part of keeping software running correctly.  This program
can emit metrics (see the configuration to learn how to enable it), which can
then be scraped by a Prometheus server.

Metrics to pay attention to:

* `http_server_active_requests`: if this gauge is high, you may be under a
  denial of service attack.
* `http_server_request_duration_seconds_count`: any time series with an
   `http_response_status_code` other than 200 is usually indication that
   there is a problem with the backend accessed by the specific proxy
   indicated by the `server_address` and `http_route`.  504 status codes
   indicate the backend server failed to respond on time, while 502 status
   codes indicate the backend server is down or refusing to accept requests.

## License

This software is distributed under the provisions of the
[Apache 2.0 license](./LICENSE).

## Contributions

We appreciate anyone who wants to contribute to this project.  Our
contributions follow the standard Github model -- you can create issues,
fork the project, and send pull requests.
