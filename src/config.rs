use duration_string::DurationString;
use regex;
use serde;
use serde::de::Error;
use serde::Deserialize;
use serde::Deserializer;
use serde_enum_str::{Deserialize_enum_str, Serialize_enum_str};
use serde_yaml::{self};
use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

#[derive(Deserialize_enum_str, Serialize_enum_str, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "snake_case")]
// Protocol
pub enum Method {
    Http,
    Https,
}
impl Default for Method {
    fn default() -> Self {
        Method::Http
    }
}

/* FIXME: implement HTTPS */
#[derive(Deserialize_enum_str, Serialize_enum_str, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "snake_case")]
// Protocol. What's the different between these? what required
pub enum ServerMethod {
    Http,
}
impl Default for ServerMethod {
    fn default() -> Self {
        ServerMethod::Http
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
// Don't we just want to "allow" certain metrics. would it be enough to just have single allow filter to reduce the amount of code?
pub enum ConfigLabelFilterAction {
    Keep,
    Drop,
    Cache { duration: DurationString },
}

// Why not just let the user specify the full regex however they want and keep the code simpler? users can always hack this by adding .* on stard/end
fn anchored_regex<'de, D>(deserializer: D) -> Result<regex::Regex, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    let real = "^".to_string() + &s.to_string() + &"$".to_string();
    match regex::Regex::new(real.as_str()) {
        Ok(regex) => Ok(regex),
        Err(err) => Err(D::Error::custom(err)),
    }
}

fn default_source_labels() -> Vec<String> {
    vec!["__name__".to_string()]
}

fn default_label_separator() -> String {
    ";".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct ConfigLabelFilter {
    // if i understand correctly, we're applying actions based on labels to drop certain metrics or not.
    // it seems it's mostly useful to drop metrics based on their name, and not any other label so i'm not sure if we need something like this
    // this design seems to complicate the code a fair bit, although it is more generic than separately filtering by label / name
    // Side note, it might be needed to drop labels from certain metrics instead of filtering on labels, but i don't have any information about this and would wait for until and if we need it to implement it.
    #[serde(default = "default_source_labels")]
    pub source_labels: Vec<String>,
    #[serde(default = "default_label_separator")]
    pub separator: String,
    #[serde(deserialize_with = "anchored_regex")]
    pub regex: regex::Regex,
    pub actions: Vec<ConfigLabelFilterAction>,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "ConfigListenOnInConfigFile")]
pub struct ConfigListenOn {
    // Protocol
    // Instead it might be better to specify tlsConfig, if specified, server automatically serves only https requests instead
    pub method: ServerMethod,
    // looks like it would be easier to accept just a string like in the example: https://github.com/tokio-rs/axum#usage-example
    // then you wouldn't need additional 80 lines of code for converting host and port from string into concrete types
    pub host: IpAddr,
    pub port: u16,
    // Can we use the same value timeouts for both header and read?
    #[serde(default = "default_header_read_timeout")]
    pub header_read_timeout: DurationString,
    // maybe just use timeout_seconds? probably not necessary to define this timeout in any other unit of time
    #[serde(default = "default_request_response_timeout")]
    pub request_response_timeout: DurationString,
    pub handler: String,
}

#[derive(Debug, Deserialize)]
struct ConfigListenOnInConfigFile {
    // Protocol
    #[serde(default)]
    method: ServerMethod,
    address: String,
    handler: String,
    #[serde(default = "default_header_read_timeout")]
    header_read_timeout: DurationString,
    #[serde(default = "default_request_response_timeout")]
    request_response_timeout: DurationString,
}

enum ConfigListenOnParseError {
    ParseIntError(std::num::ParseIntError),
    AddrParseError(std::net::AddrParseError),
    OutOfBoundsError(String),
}

impl std::fmt::Display for ConfigListenOnParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigListenOnParseError::ParseIntError(e) => {
                write!(f, "port not an integer: {}", e)
            }
            ConfigListenOnParseError::AddrParseError(e) => {
                write!(f, "invalid listen address: {}", e)
            }
            ConfigListenOnParseError::OutOfBoundsError(e) => {
                write!(f, "port out of bounds: {}", e)
            }
        }
    }
}

impl From<std::num::ParseIntError> for ConfigListenOnParseError {
    fn from(err: std::num::ParseIntError) -> Self {
        ConfigListenOnParseError::ParseIntError(err)
    }
}

impl From<std::net::AddrParseError> for ConfigListenOnParseError {
    fn from(err: std::net::AddrParseError) -> Self {
        ConfigListenOnParseError::AddrParseError(err)
    }
}

impl TryFrom<ConfigListenOnInConfigFile> for ConfigListenOn {
    type Error = ConfigListenOnParseError;

    fn try_from(other: ConfigListenOnInConfigFile) -> Result<Self, Self::Error> {
        let mut host = IpAddr::from_str("0.0.0.0")?;
        let port: u16;
        let parts: Vec<&str> = other.address.rsplit(":").collect();
        if parts.len() == 1 {
            port = parts[0].parse()?;
        } else {
            if parts[1] != "" {
                host = IpAddr::from_str(parts[1])?;
            }
            port = parts[0].parse()?
        }
        if port < 1024 {
            return Err(ConfigListenOnParseError::OutOfBoundsError(format!(
                "{}",
                parts[0]
            )));
        }
        Ok(ConfigListenOn {
            method: other.method,
            host: host,
            port: port,
            handler: other.handler,
            header_read_timeout: other.header_read_timeout,
            request_response_timeout: other.request_response_timeout,
        })
    }
}

fn default_timeout() -> DurationString {
    DurationString::new(Duration::new(30, 0))
}

fn default_header_read_timeout() -> DurationString {
    DurationString::new(Duration::new(5, 0))
}

fn default_request_response_timeout() -> DurationString {
    let df: Duration = default_timeout().into();
    DurationString::new(df + Duration::new(5, 0))
}

#[derive(Debug, Deserialize, Clone)]
pub struct ConfigConnectTo {
    // protocol
    // I think i wrote elsewhere that it's probably easier to just configure url instead of 3 different fields
    #[serde(default)]
    pub method: Method,
    pub address: String,
    pub handler: String,
    #[serde(default = "default_timeout")]
    pub timeout: DurationString,
}

#[derive(Debug, Deserialize)]
pub struct ConfigProxyEntry {
    pub listen_on: ConfigListenOn,
    pub connect_to: ConfigConnectTo,
    pub label_filters: Vec<ConfigLabelFilter>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub proxies: Vec<ConfigProxyEntry>,
}

#[derive(Debug)]
pub enum LoadConfigError {
    ReadError(std::io::Error),
    ParseError(serde_yaml::Error),
    ConflictingConfig(String),
    InvalidActionRegex(String),
}

impl fmt::Display for LoadConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LoadConfigError::ReadError(e) => write!(f, "cannot read configuration: {}", e),
            LoadConfigError::ParseError(e) => write!(f, "cannot parse configuration: {}", e),
            LoadConfigError::ConflictingConfig(e) => write!(f, "conflicting configuration: {}", e),
            LoadConfigError::InvalidActionRegex(e) => {
                write!(f, "invalid action regular expression: {}", e)
            }
        }
    }
}

impl From<std::io::Error> for LoadConfigError {
    fn from(err: std::io::Error) -> Self {
        LoadConfigError::ReadError(err)
    }
}

impl From<serde_yaml::Error> for LoadConfigError {
    fn from(err: serde_yaml::Error) -> Self {
        LoadConfigError::ParseError(err)
    }
}

pub fn load_config(path: PathBuf) -> Result<Config, LoadConfigError> {
    let f = std::fs::File::open(path.clone())?;
    let maybecfg: Result<Config, serde_yaml::Error> = serde_yaml::from_reader(f);
    if let Err(error) = maybecfg {
        return Err(LoadConfigError::ParseError(error));
    }
    let cfg = maybecfg.unwrap();
    struct IndexAndMethod {
        index: usize,
        method: ServerMethod,
    }
    let mut by_host_port_handler = std::collections::HashMap::new();
    let mut by_host_port: HashMap<String, IndexAndMethod> = std::collections::HashMap::new();
    for (index, element) in cfg.proxies.iter().enumerate() {
        let host_port = format!("{}:{}", element.listen_on.host, element.listen_on.port);
        let host_port_handler = format!(
            "{}:{}/{}",
            element.listen_on.host, element.listen_on.port, element.listen_on.handler
        );
        if let Some(priorindex) = by_host_port_handler.get(&host_port_handler) {
            return Err(LoadConfigError::ConflictingConfig(
                format!(
                    "proxy {} in configuration proxies list contains the same host, port and handler as proxy {}; two proxies cannot listen on the same HTTP handler simultaneously",
                    priorindex + 1, index + 1
                )
            ));
        } else {
            by_host_port_handler.insert(host_port_handler, index);
        }
        if let Some(prior) = by_host_port.get(&host_port) {
            if element.listen_on.method != prior.method {
                return Err(LoadConfigError::ConflictingConfig(
                    format!(
                        "proxy {} in configuration proxies list has an HTTP method conflicting with proxy {} listening on the same host and port; the same listening address cannot serve both HTTP and HTTPS at the same time",
                        prior.index + 1, index + 1
                    )
                ));
            }
        } else {
            by_host_port.insert(
                host_port,
                IndexAndMethod {
                    index: index,
                    method: element.listen_on.method.clone(),
                },
            );
        }
    }
    Ok(cfg)
}

// Both of these configs are basically copies of existing structs in config. We should just create a server directly from those configs
#[derive(Debug, Clone)]
pub struct HttpProxyTarget {
    pub connect_to: ConfigConnectTo,
    pub label_filters: Vec<ConfigLabelFilter>,
}
#[derive(Debug, Clone)]
pub struct HttpProxy {
    method: ServerMethod,
    pub host: IpAddr,
    pub port: u16,
    pub header_read_timeout: Duration,
    pub request_response_timeout: Duration,
    pub handlers: HashMap<String, HttpProxyTarget>,
}

// what's the purpose of this function? it seems like it's just copying the same data into mostly the same struct
pub fn convert_config_to_proxy_list(config: Config) -> Vec<HttpProxy> {
    let mut servers: HashMap<String, HttpProxy> = HashMap::new();
    for proxy in config.proxies {
        let listen_on = proxy.listen_on;
        let serveraddr = format!("{}:{}", listen_on.host, listen_on.port);
        if !servers.contains_key(&serveraddr) {
            servers.insert(
                String::from_str(&serveraddr).unwrap(),
                HttpProxy {
                    method: listen_on.method,
                    host: listen_on.host,
                    port: listen_on.port,
                    header_read_timeout: listen_on.header_read_timeout.into(),
                    request_response_timeout: listen_on.request_response_timeout.into(),
                    handlers: HashMap::new(),
                },
            );
        }
        if !servers
            .get(&serveraddr)
            .unwrap()
            .handlers
            .contains_key(&listen_on.handler)
        {
            let newhandlers = HashMap::from([(
                listen_on.handler.clone(),
                HttpProxyTarget {
                    connect_to: proxy.connect_to,
                    label_filters: proxy.label_filters,
                },
            )]);
            let oldserver = servers.remove(&serveraddr).unwrap();
            let concathandlers = oldserver
                .handlers
                .clone()
                .into_iter()
                .chain(newhandlers)
                .collect();
            servers.insert(
                String::from_str(&serveraddr).unwrap(),
                HttpProxy {
                    method: oldserver.method,
                    host: oldserver.host.clone(),
                    port: oldserver.port,
                    header_read_timeout: oldserver.header_read_timeout.clone(),
                    request_response_timeout: oldserver.request_response_timeout.clone(),
                    handlers: concathandlers,
                },
            );
        }
    }
    servers.values().cloned().collect()
}
