use duration_string::DurationString;
use regex;
use serde;
use serde::de::Error;
use serde::Deserialize;
use serde::Deserializer;
use serde_yaml::{self};
use std::collections::HashMap;
use std::fmt;
use std::io::Cursor;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use url::Url;

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub enum Protocol {
    #[default]
    Http,
    Https {
        certificate: Vec<rustls::Certificate>,
        key: rustls::PrivateKey,
    },
}

impl TryFrom<&ListenOn> for Protocol {
    type Error = ListenOnParseError;
    fn try_from(other: &ListenOn) -> Result<Self, Self::Error> {
        let mut scheme = other.url.scheme();
        if scheme.is_empty() {
            scheme = "http";
        }

        match scheme {
            "http" => {
                if other.certificate_file.is_some() || other.key_file.is_some() {
                    Err(Self::Error::SSLOptionsNotAllowed)
                } else {
                    Ok(Self::Http)
                }
            }
            "https" => {
                if other.certificate_file.is_none() {
                    return Err(Self::Error::CertificateFileRequired);
                }
                if other.key_file.is_none() {
                    return Err(Self::Error::KeyFileRequired);
                }
                let certdata = std::fs::read(other.certificate_file.clone().unwrap());
                if let Err(err) = certdata {
                    return Err(Self::Error::CertificateFileReadError(err));
                }
                let keydata = std::fs::read(other.key_file.clone().unwrap());
                if let Err(err) = keydata {
                    return Err(Self::Error::KeyFileReadError(err));
                }

                let mut certs_cursor: Cursor<Vec<u8>> = Cursor::new(certdata.unwrap());
                let certs_loaded = rustls_pemfile::certs(&mut certs_cursor);
                if let Err(err) = certs_loaded {
                    return Err(Self::Error::CertificateFileReadError(err));
                }
                let certs_parsed: Vec<rustls::Certificate> = certs_loaded
                    .unwrap()
                    .into_iter()
                    .map(rustls::Certificate)
                    .collect();
                if certs_parsed.is_empty() {
                    return Err(Self::Error::CertificateFileReadError(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!(
                            "{} contains no certificates",
                            other.certificate_file.clone().unwrap().display()
                        ),
                    )));
                }

                let mut key_cursor = Cursor::new(keydata.unwrap());
                let mut keys_loaded: Vec<Vec<u8>> = vec![];

                match rustls_pemfile::pkcs8_private_keys(&mut key_cursor) {
                    Err(err) => return Err(Self::Error::KeyFileReadError(err)),
                    Ok(res) => keys_loaded.extend(res),
                }
                key_cursor.set_position(0);
                match rustls_pemfile::rsa_private_keys(&mut key_cursor) {
                    Err(err) => return Err(Self::Error::KeyFileReadError(err)),
                    Ok(res) => keys_loaded.extend(res),
                }
                key_cursor.set_position(0);
                match rustls_pemfile::ec_private_keys(&mut key_cursor) {
                    Err(err) => return Err(Self::Error::KeyFileReadError(err)),
                    Ok(res) => keys_loaded.extend(res),
                }

                if keys_loaded.len() != 1 {
                    return Err(Self::Error::KeyFileReadError(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!(
                            "{} contains {} keys whereas it should contain only 1",
                            other.key_file.clone().unwrap().display(),
                            keys_loaded.len(),
                        ),
                    )));
                }

                Ok(Self::Https {
                    certificate: certs_parsed,
                    key: rustls::PrivateKey(keys_loaded[0].clone()),
                })
            }
            _ => Err(Self::Error::InvalidURL(InvalidURLError::UnsupportedScheme(
                scheme.to_owned(),
            ))),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
/// All possible actions to apply to metrics as part of a client request.
/// Actions in a list of actions are processed from first to last.
pub enum LabelFilterAction {
    /// Keep the metric.
    Keep,
    /// Drop the metric.
    Drop,
    /// Cache the metric for an amount of time.
    ReduceTimeResolution { resolution: DurationString },
}

fn anchored_regex<'de, D>(deserializer: D) -> Result<regex::Regex, D::Error>
where
    D: Deserializer<'de>,
{
    // This regex is to be anchored to ensure people familiar with
    // Prometheus rewrite rules (which this program is inspired by)
    // do not encounter surprises like overmatching.
    let s: String = Deserialize::deserialize(deserializer)?;
    let real = "^".to_string() + &s.to_string() + "$";
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
#[serde(deny_unknown_fields)]
/// Match each returned time series (to be processed) according to
/// the listed labels, concatenated according to the separator,
/// and matching with the specified regular expression, anchored
/// at beginning and end.
pub struct LabelFilter {
    #[serde(default = "default_source_labels")]
    pub source_labels: Vec<String>,
    #[serde(default = "default_label_separator")]
    pub separator: String,
    #[serde(deserialize_with = "anchored_regex")]
    pub regex: regex::Regex,
    pub actions: Vec<LabelFilterAction>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(try_from = "ListenOn")]
pub struct ListenerSpec {
    pub protocol: Protocol,
    pub sockaddr: SocketAddr,
    pub header_read_timeout: Duration,
    pub request_response_timeout: Duration,
    pub handler: String,
}

enum InvalidURLError {
    AddrParseError(std::net::AddrParseError),
    AddrResolveError(std::io::Error),
    InvalidAddressError(String),
    UnsupportedScheme(String),
    AuthenticationUnsupported,
    FragmentUnsupported,
}

impl std::fmt::Display for InvalidURLError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::AddrParseError(e) => {
                write!(f, "cannot parse address: {e}")
            }
            Self::AddrResolveError(e) => {
                write!(f, "cannot resolve address: {e}")
            }
            Self::InvalidAddressError(e) => {
                write!(f, "invalid address: {e}")
            }
            Self::UnsupportedScheme(scheme) => {
                write!(f, "the {scheme} protocol is not supported by this program",)
            }
            Self::AuthenticationUnsupported => {
                write!(f, "authentication is currently not supported")
            }
            Self::FragmentUnsupported => {
                write!(f, "fragments may not be specified")
            }
        }
    }
}

fn default_header_read_timeout() -> DurationString {
    DurationString::new(Duration::new(5, 0))
}

fn default_request_response_timeout() -> DurationString {
    let df: Duration = default_timeout().into();
    DurationString::new(df + Duration::new(5, 0))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Specifies which host and port to listen on, and on which
/// HTTP handler (path) to respond to.
struct ListenOn {
    url: Url,
    certificate_file: Option<std::path::PathBuf>,
    key_file: Option<std::path::PathBuf>,
    #[serde(default = "default_header_read_timeout")]
    header_read_timeout: DurationString,
    #[serde(default = "default_request_response_timeout")]
    request_response_timeout: DurationString,
}

enum ListenOnParseError {
    InvalidURL(InvalidURLError),
    PortMissing,
    PortOutOfBoundsError(u16),
    QueryStringUnsupported,
    CertificateFileRequired,
    KeyFileRequired,
    CertificateFileReadError(std::io::Error),
    KeyFileReadError(std::io::Error),
    SSLOptionsNotAllowed,
}

impl std::fmt::Display for ListenOnParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidURL(e) => {
                write!(f, "listen URL not valid: {e}")
            }
            Self::PortMissing => {
                write!(f, "port missing from listen URL")
            }
            Self::PortOutOfBoundsError(e) => {
                write!(f, "port in listen URL out of bounds: {e}")
            }
            Self::QueryStringUnsupported => {
                write!(f, "query strings may not be specified in listen URL")
            }
            Self::CertificateFileRequired => {
                write!(f, "certificate_file is required for HTTPS")
            }
            Self::KeyFileRequired => {
                write!(f, "key_file is required for HTTPS https")
            }
            Self::CertificateFileReadError(e) => {
                write!(f, "could not read certificate file: {e}")
            }
            Self::KeyFileReadError(e) => {
                write!(f, "could not read key file: {e}")
            }
            Self::SSLOptionsNotAllowed => {
                write!(
                    f,
                    "options certificate_file and key_file are not allowed when serving plain HTTP"
                )
            }
        }
    }
}

impl From<std::net::AddrParseError> for ListenOnParseError {
    fn from(err: std::net::AddrParseError) -> Self {
        ListenOnParseError::InvalidURL(InvalidURLError::AddrParseError(err))
    }
}

impl From<std::io::Error> for ListenOnParseError {
    fn from(err: std::io::Error) -> Self {
        ListenOnParseError::InvalidURL(InvalidURLError::AddrResolveError(err))
    }
}

impl TryFrom<ListenOn> for ListenerSpec {
    type Error = ListenOnParseError;

    fn try_from(other: ListenOn) -> Result<Self, Self::Error> {
        let hostport = format!(
            "{}:{}",
            match other.url.host() {
                Some(h) => h.to_string(),
                None => "0.0.0.0".to_string(),
            },
            match other.url.port() {
                Some(p) => {
                    if p < 1024 {
                        return Err(Self::Error::PortOutOfBoundsError(p));
                    }
                    p
                }
                None => {
                    return Err(Self::Error::PortMissing);
                }
            }
        );
        let Some(sockaddr) = hostport.to_socket_addrs()?.next() else {
            return Err(Self::Error::InvalidURL(
                InvalidURLError::InvalidAddressError(hostport),
            ));
        };

        if !other.url.username().is_empty() || other.url.password().is_some() {
            return Err(Self::Error::InvalidURL(
                InvalidURLError::AuthenticationUnsupported,
            ));
        }
        if other.url.query().is_some() {
            return Err(Self::Error::QueryStringUnsupported);
        }
        if other.url.fragment().is_some() {
            return Err(Self::Error::InvalidURL(
                InvalidURLError::FragmentUnsupported,
            ));
        }
        let proto = Protocol::try_from(&other)?;

        Ok(ListenerSpec {
            protocol: proto,
            sockaddr,
            handler: other.url.path().to_owned(),
            header_read_timeout: other.header_read_timeout.into(),
            request_response_timeout: other.request_response_timeout.into(),
        })
    }
}

fn default_timeout() -> DurationString {
    DurationString::new(Duration::new(30, 0))
}

#[derive(Debug, Deserialize, Clone)]
#[serde(remote = "Self")]
/// Indicates to the proxy which backend server to fetch metrics from.
pub struct ConnectTo {
    pub url: Url,
    #[serde(default = "default_timeout")]
    pub timeout: DurationString,
}

enum ConnectToParseError {
    InvalidURL(InvalidURLError),
}

impl std::fmt::Display for ConnectToParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidURL(e) => {
                write!(f, "connect URL not valid: {e}")
            }
        }
    }
}

impl<'de> Deserialize<'de> for ConnectTo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let other = ConnectTo::deserialize(deserializer)?;
        if !other.url.username().is_empty() || other.url.password().is_some() {
            return Err(serde::de::Error::custom(ConnectToParseError::InvalidURL(
                InvalidURLError::AuthenticationUnsupported,
            )));
        }
        if other.url.fragment().is_some() {
            return Err(serde::de::Error::custom(ConnectToParseError::InvalidURL(
                InvalidURLError::FragmentUnsupported,
            )));
        }
        let scheme = other.url.scheme();
        match scheme {
            "http" | "https" => {}
            _ => {
                return Err(serde::de::Error::custom(ConnectToParseError::InvalidURL(
                    InvalidURLError::UnsupportedScheme(scheme.to_owned()),
                )));
            }
        }

        Ok(other)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxyEntry {
    listen_on: ListenerSpec,
    connect_to: ConnectTo,
    label_filters: Vec<LabelFilter>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    proxies: Vec<ProxyEntry>,
}

#[derive(Debug)]
pub enum LoadError {
    ReadError(std::io::Error),
    ParseError(serde_yaml::Error),
    ConflictingConfig(String),
    InvalidActionRegex(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LoadError::ReadError(e) => write!(f, "cannot read configuration: {e}"),
            LoadError::ParseError(e) => write!(f, "cannot parse configuration: {e}"),
            LoadError::ConflictingConfig(e) => write!(f, "conflicting configuration: {e}"),
            LoadError::InvalidActionRegex(e) => {
                write!(f, "invalid action regular expression: {e}")
            }
        }
    }
}

impl From<std::io::Error> for LoadError {
    fn from(err: std::io::Error) -> Self {
        LoadError::ReadError(err)
    }
}

impl From<serde_yaml::Error> for LoadError {
    fn from(err: serde_yaml::Error) -> Self {
        LoadError::ParseError(err)
    }
}

impl TryFrom<PathBuf> for Config {
    type Error = LoadError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        struct IndexAndProtocol {
            index: usize,
            protocol: Protocol,
        }
        let f = std::fs::File::open(path.clone())?;
        let maybecfg: Result<Config, serde_yaml::Error> = serde_yaml::from_reader(f);
        if let Err(error) = maybecfg {
            return Err(Self::Error::ParseError(error));
        }
        let cfg = maybecfg.unwrap();
        let mut by_host_port_handler = std::collections::HashMap::new();
        let mut by_host_port: HashMap<String, IndexAndProtocol> = std::collections::HashMap::new();
        for (index, element) in cfg.proxies.iter().enumerate() {
            let host_port = format!("{}", element.listen_on.sockaddr);
            let host_port_handler = format!(
                "{}/{}",
                element.listen_on.sockaddr, element.listen_on.handler
            );
            if let Some(priorindex) = by_host_port_handler.get(&host_port_handler) {
                return Err(Self::Error::ConflictingConfig(
                    format!(
                        "proxy {} in configuration proxies list contains the same host, port and handler as proxy {}; two proxies cannot listen on the same HTTP handler simultaneously",
                        priorindex + 1, index + 1
                    )
                ));
            }
            by_host_port_handler.insert(host_port_handler, index);

            if let Some(prior) = by_host_port.get(&host_port) {
                if let Protocol::Https {
                    certificate: firstcert,
                    key: firstkey,
                } = element.listen_on.protocol.clone()
                {
                    if let Protocol::Https {
                        certificate: secondcert,
                        key: secondkey,
                    } = prior.protocol.clone()
                    {
                        if firstcert != secondcert {
                            return Err(Self::Error::ConflictingConfig(
                                format!(
                                    "proxy {} uses a different certificate from proxy {}; the same listening address must use the same certificate",
                                    prior.index + 1, index + 1
                                )
                            ));
                        }
                        if firstkey != secondkey {
                            return Err(Self::Error::ConflictingConfig(
                                format!(
                                    "proxy {} uses a different private key from proxy {}; the same listening address must use the same private key",
                                    prior.index + 1, index + 1
                                )
                            ));
                        }
                    }
                }
                if element.listen_on.protocol != prior.protocol {
                    return Err(Self::Error::ConflictingConfig(
                        format!(
                            "proxy {} in configuration proxies list uses a protocol conflicting with proxy {} listening on the same host and port; the same listening address cannot serve both HTTP and HTTPS at the same time",
                            prior.index + 1, index + 1
                        )
                    ));
                }
            } else {
                by_host_port.insert(
                    host_port,
                    IndexAndProtocol {
                        index,
                        protocol: element.listen_on.protocol.clone(),
                    },
                );
            }
        }
        Ok(cfg)
    }
}

#[derive(Debug, Clone)]
pub struct HttpProxyTarget {
    pub connect_to: ConnectTo,
    pub label_filters: Vec<LabelFilter>,
}
#[derive(Debug, Clone)]
pub struct HttpProxy {
    pub listen_on: ListenerSpec,
    pub handlers: HashMap<String, HttpProxyTarget>,
}

impl From<Config> for Vec<HttpProxy> {
    fn from(val: Config) -> Self {
        // This function is necessary because a config may specify multiple
        // listeners all on the same port and IP, each one with a different
        // proxy target, but the HTTP server cannot be told to listen to
        // the same host and port twice, so we have to group the configs
        // by listen port + listen IP.
        let mut servers: HashMap<String, HttpProxy> = HashMap::new();
        for proxy in val.proxies {
            let listen_on = proxy.listen_on;
            let handler = listen_on.handler.clone();
            let serveraddr = format!("{}", listen_on.sockaddr);

            if !servers.contains_key(&serveraddr) {
                servers.insert(
                    String::from_str(&serveraddr).unwrap(),
                    HttpProxy {
                        listen_on: listen_on,
                        handlers: HashMap::new(),
                    },
                );
            }

            if !servers
                .get(&serveraddr)
                .unwrap()
                .handlers
                .contains_key(&handler)
            {
                let oldserver = servers.remove(&serveraddr).unwrap();
                servers.insert(
                    String::from_str(&serveraddr).unwrap(),
                    HttpProxy {
                        listen_on: oldserver.listen_on,
                        handlers: oldserver
                            .handlers
                            .into_iter()
                            .chain(HashMap::from([(
                                handler,
                                HttpProxyTarget {
                                    connect_to: proxy.connect_to,
                                    label_filters: proxy.label_filters,
                                },
                            )]))
                            .collect(),
                    },
                );
            }
        }
        servers.values().cloned().collect()
    }
}
