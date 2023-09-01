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
use std::io::Cursor;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

#[derive(Deserialize_enum_str, Serialize_enum_str, Debug, PartialEq, Eq, Clone)]
#[serde(rename_all = "snake_case")]
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
pub enum ServerMethod {
    Http,
    Https,
}
impl Default for ServerMethod {
    fn default() -> Self {
        ServerMethod::Http
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLabelFilterAction {
    Keep,
    Drop,
    Cache { duration: DurationString },
}

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
    pub method: ServerMethod,
    pub certificate: Option<Vec<rustls::Certificate>>,
    pub key: Option<rustls::PrivateKey>,
    pub host: IpAddr,
    pub port: u16,
    #[serde(default = "default_header_read_timeout")]
    pub header_read_timeout: DurationString,
    #[serde(default = "default_request_response_timeout")]
    pub request_response_timeout: DurationString,
    pub handler: String,
}

#[derive(Debug, Deserialize)]
struct ConfigListenOnInConfigFile {
    #[serde(default)]
    method: ServerMethod,
    certificate_file: Option<std::path::PathBuf>,
    key_file: Option<std::path::PathBuf>,
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
    CertificateFileRequired,
    KeyFileRequired,
    CertificateFileReadError(std::io::Error),
    KeyFileReadError(std::io::Error),
    SSLOptionsNotAllowed,
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
            ConfigListenOnParseError::CertificateFileRequired => {
                write!(
                    f,
                    "certificate_file is required when listen method is https"
                )
            }
            ConfigListenOnParseError::KeyFileRequired => {
                write!(f, "key_file is required when listen method is https")
            }
            ConfigListenOnParseError::CertificateFileReadError(e) => {
                write!(f, "could not read certificate file: {}", e)
            }
            ConfigListenOnParseError::KeyFileReadError(e) => {
                write!(f, "could not read key file: {}", e)
            }
            ConfigListenOnParseError::SSLOptionsNotAllowed => {
                write!(f, "options certificate_file and key_file are not supported when listen method is http")
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
        let mut certs: Option<Vec<rustls::Certificate>> = None;
        let mut key: Option<rustls::PrivateKey> = None;
        match other.method {
            ServerMethod::Http => {
                if let Some(_) = other.certificate_file {
                    return Err(ConfigListenOnParseError::SSLOptionsNotAllowed);
                }
                if let Some(_) = other.key_file {
                    return Err(ConfigListenOnParseError::SSLOptionsNotAllowed);
                }
            }
            ServerMethod::Https => {
                if let None = other.certificate_file {
                    return Err(ConfigListenOnParseError::CertificateFileRequired);
                }
                if let None = other.key_file {
                    return Err(ConfigListenOnParseError::KeyFileRequired);
                }
                let certdata = std::fs::read(other.certificate_file.clone().unwrap());
                if let Err(err) = certdata {
                    return Err(ConfigListenOnParseError::CertificateFileReadError(err));
                }
                let keydata = std::fs::read(other.key_file.clone().unwrap());
                if let Err(err) = keydata {
                    return Err(ConfigListenOnParseError::KeyFileReadError(err));
                }

                let mut certs_cursor: Cursor<Vec<u8>> = Cursor::new(certdata.unwrap());
                let certs_loaded = rustls_pemfile::certs(&mut certs_cursor);
                if let Err(err) = certs_loaded {
                    return Err(ConfigListenOnParseError::CertificateFileReadError(err));
                }
                let certs_parsed: Vec<rustls::Certificate> = certs_loaded
                    .unwrap()
                    .into_iter()
                    .map(rustls::Certificate)
                    .collect();
                if certs_parsed.len() < 1 {
                    return Err(ConfigListenOnParseError::CertificateFileReadError(
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!(
                                "{} contains no certificates",
                                other.certificate_file.clone().unwrap().display()
                            ),
                        ),
                    ));
                }

                let mut key_cursor = Cursor::new(keydata.unwrap());
                let mut keys_loaded: Vec<Vec<u8>> = vec![];

                match rustls_pemfile::pkcs8_private_keys(&mut key_cursor) {
                    Err(err) => return Err(ConfigListenOnParseError::KeyFileReadError(err)),
                    Ok(res) => keys_loaded.extend(res),
                }

                match rustls_pemfile::rsa_private_keys(&mut key_cursor) {
                    Err(err) => return Err(ConfigListenOnParseError::KeyFileReadError(err)),
                    Ok(res) => keys_loaded.extend(res),
                }

                match rustls_pemfile::ec_private_keys(&mut key_cursor) {
                    Err(err) => return Err(ConfigListenOnParseError::KeyFileReadError(err)),
                    Ok(res) => keys_loaded.extend(res),
                }

                if keys_loaded.len() != 1 {
                    return Err(ConfigListenOnParseError::KeyFileReadError(
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!(
                                "{} contains {} keys whereas it should contain only 1",
                                other.key_file.clone().unwrap().display(),
                                keys_loaded.len(),
                            ),
                        ),
                    ));
                }

                certs = Some(certs_parsed);
                key = Some(rustls::PrivateKey(keys_loaded[0].clone()));
            }
        }

        Ok(ConfigListenOn {
            method: other.method,
            certificate: certs,
            key: key,
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

#[derive(Debug, Clone)]
pub struct HttpProxyTarget {
    pub connect_to: ConfigConnectTo,
    pub label_filters: Vec<ConfigLabelFilter>,
}
#[derive(Debug, Clone)]
pub struct HttpProxy {
    pub method: ServerMethod,
    pub certificate: Option<Vec<rustls::Certificate>>,
    pub key: Option<rustls::PrivateKey>,
    pub host: IpAddr,
    pub port: u16,
    pub header_read_timeout: Duration,
    pub request_response_timeout: Duration,
    pub handlers: HashMap<String, HttpProxyTarget>,
}

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
                    certificate: listen_on.certificate,
                    key: listen_on.key,
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
                    certificate: oldserver.certificate,
                    key: oldserver.key,
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
