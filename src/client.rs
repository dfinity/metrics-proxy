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
// HttpResult
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
    // HttpError
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
    // consider using url package
    // c.method should be named protocol. got confused by the naming in some other places as well
    let url = format!("{}://{}{}", c.method, c.address, c.handler);
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .headers(h)
        .timeout(c.timeout.into())
        .send()
        .await?;
    // Seems like these variables are mostly used ones and don't need to be extracted into variables
    let status = response.status().clone();
    let headers = response.headers().clone();
    let text = response.text().await?;
    // if let status = response.status(); status != reqwest::StatusCode::OK
    if status != reqwest::StatusCode::OK {
        return Err(ScrapeError::Non200(Non200Result {
            status: status,
            headers: headers,
            data: text,
        }));
    }
    let lines: Vec<_> = text.lines().map(|s| Ok(s.to_owned())).collect();
    // no need to create a variable which is used only once
    let maybe_parsed = prometheus_parse::Scrape::parse(lines.into_iter());
    match maybe_parsed {
        Ok(parsed) => Ok(ScrapeResult {
            headers: headers,
            series: parsed,
        }),
        Err(err) => Err(ScrapeError::ParseError(err)),
    }
}
