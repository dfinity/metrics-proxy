use prometheus_parse;
use reqwest;
use reqwest::header;

/*
FIXME:
copy all headers from client request to server response
except for the fields that have obviously changed such
as length which must be recomputed.
*/

#[derive(Debug)]
pub struct Non200Result {
    pub status: reqwest::StatusCode,
    pub headers: header::HeaderMap,
    pub data: String,
}

pub struct ScrapeResult {
    pub headers: header::HeaderMap,
    pub series: prometheus_parse::Scrape,
}

#[derive(Debug)]
pub enum ScrapeError {
    Non200(Non200Result),
    FetchError(reqwest::Error),
    ParseError(std::io::Error),
}

impl From<reqwest::Error> for ScrapeError {
    fn from(err: reqwest::Error) -> Self {
        ScrapeError::FetchError(err)
    }
}

impl From<std::io::Error> for ScrapeError {
    fn from(err: std::io::Error) -> Self {
        ScrapeError::ParseError(err)
    }
}

impl From<Non200Result> for ScrapeError {
    fn from(err: Non200Result) -> Self {
        ScrapeError::Non200(err)
    }
}

pub async fn scrape(
    c: &crate::config::ConfigConnectTo,
    h: reqwest::header::HeaderMap,
) -> Result<ScrapeResult, ScrapeError> {
    let url = format!("{}://{}{}", c.protocol, c.address, c.handler);
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .headers(h)
        .timeout(c.timeout.into())
        .send()
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await?;
    if status != reqwest::StatusCode::OK {
        return Err(ScrapeError::Non200(Non200Result {
            status,
            headers,
            data: text,
        }));
    }
    let maybe_parsed = prometheus_parse::Scrape::parse(text.lines().map(|s| Ok(s.to_owned())));
    match maybe_parsed {
        Ok(parsed) => Ok(ScrapeResult {
            headers,
            series: parsed,
        }),
        Err(err) => Err(ScrapeError::ParseError(err)),
    }
}
