
- seems like method, host, port distinction is made to enable verifying the configuration before the server starts
    - for our use-case, this config file would include a handful of proxies. it seems that effort of complicating the code is not worth it since the failure should be apparent very fast: when the server exits with an error because it failed to bound to already used endpoint.
- check clippy warnings - we have this check automated in other repos
- filtering seems more complex than it needs be
    - simple metric name regex for "allow" should suffice - i.e. anything that's matched by any regex should be kept
    - cache doesn't seem so useful
    - could instead remove some label values but would wait to identify if we really need it. likely we should be able to ship without it and hopefully live without it
