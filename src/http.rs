//! The shared HTTP client for all transcript requests.

use std::error::Error as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::cookie::Jar;
use reqwest::header::{ACCEPT_LANGUAGE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Proxy, StatusCode, Url};

use crate::error::FetchError;

/// One realistic desktop User-Agent for the full run.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/141.0.0.0 Safari/537.36";
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpSetupError {
    #[error("the proxy URL is not valid: {0}")]
    Proxy(reqwest::Error),
    #[error("cannot make the HTTP client: {0}")]
    Build(reqwest::Error),
}

/// A successful response.
#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub(crate) body: String,
}

pub(crate) struct HttpClient {
    client: Client,
    jar: Arc<Jar>,
}

impl HttpClient {
    pub(crate) fn new(proxy: Option<&str>) -> Result<Self, HttpSetupError> {
        let jar = Arc::new(Jar::default());
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
        let mut builder = Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .cookie_provider(Arc::clone(&jar))
            .gzip(true)
            .timeout(TIMEOUT);
        if let Some(proxy) = proxy {
            builder = builder.proxy(Proxy::all(proxy).map_err(HttpSetupError::Proxy)?);
        }
        let client = builder.build().map_err(HttpSetupError::Build)?;
        Ok(Self { client, jar })
    }

    pub(crate) fn add_cookie(&self, cookie: &str, url: &Url) {
        self.jar.add_cookie_str(cookie, url);
    }

    pub(crate) fn get(&self, url: &str) -> Result<HttpResponse, FetchError> {
        self.send("GET", self.client.get(url))
    }

    pub(crate) fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<HttpResponse, FetchError> {
        self.send("POST", self.client.post(url).json(body))
    }

    fn send(&self, method: &str, request: RequestBuilder) -> Result<HttpResponse, FetchError> {
        let start = Instant::now();
        let response = request
            .send()
            .map_err(|error| FetchError::Transient(describe(&error)))?;
        let status = response.status();
        let final_url = response.url().clone();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);
        let body = response
            .text()
            .map_err(|error| FetchError::Transient(describe(&error)))?;
        // Log only the path. The query can contain keys and signatures.
        tracing::debug!(
            "{method} {}{} -> {} ({} bytes, {} ms)",
            final_url.host_str().unwrap_or(""),
            final_url.path(),
            status.as_u16(),
            body.len(),
            start.elapsed().as_millis()
        );
        check(status, &final_url, retry_after, body)
    }
}

/// Classify a response (PLAN.md, section 7.5).
fn check(
    status: StatusCode,
    final_url: &Url,
    retry_after: Option<Duration>,
    body: String,
) -> Result<HttpResponse, FetchError> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(FetchError::RateLimited { retry_after });
    }
    let is_google = final_url
        .host_str()
        .is_some_and(|host| host == "google.com" || host.ends_with(".google.com"));
    if is_google && final_url.path().starts_with("/sorry") {
        return Err(FetchError::Blocked(
            "redirect to the Google \"sorry\" page".to_owned(),
        ));
    }
    if body.contains("class=\"g-recaptcha\"") {
        return Err(FetchError::Blocked("reCAPTCHA page".to_owned()));
    }
    if status.is_server_error() {
        return Err(FetchError::Transient(format!("HTTP {status}")));
    }
    if status.is_success() {
        return Ok(HttpResponse { body });
    }
    Err(FetchError::Unexpected {
        message: format!("HTTP {status} from {}", final_url.path()),
        body: Some(body),
    })
}

/// `Retry-After` in seconds. The HTTP date form is not supported.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse().ok().map(Duration::from_secs)
}

/// The error and all its causes, on one line.
fn describe(error: &reqwest::Error) -> String {
    let mut text = if error.is_timeout() {
        "timeout".to_owned()
    } else {
        error.to_string()
    };
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::{StatusCode, Url};

    use super::{check, parse_retry_after};
    use crate::error::FetchError;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn classifies_429_as_rate_limited() {
        let body = include_str!("../tests/fixtures/google_sorry_429.html").to_owned();
        let result = check(
            StatusCode::TOO_MANY_REQUESTS,
            &url("https://www.youtube.com/api/timedtext?v=x"),
            Some(Duration::from_secs(60)),
            body,
        );
        assert_eq!(
            result.unwrap_err(),
            FetchError::RateLimited {
                retry_after: Some(Duration::from_secs(60))
            }
        );
    }

    #[test]
    fn classifies_sorry_redirect_and_recaptcha_as_blocked() {
        let sorry = check(
            StatusCode::OK,
            &url("https://www.google.com/sorry/index?continue=x"),
            None,
            String::new(),
        );
        assert_eq!(
            sorry.unwrap_err(),
            FetchError::Blocked("redirect to the Google \"sorry\" page".to_owned())
        );
        let captcha = check(
            StatusCode::OK,
            &url("https://www.youtube.com/watch?v=x"),
            None,
            "<div class=\"g-recaptcha\"></div>".to_owned(),
        );
        assert_eq!(
            captcha.unwrap_err(),
            FetchError::Blocked("reCAPTCHA page".to_owned())
        );
    }

    #[test]
    fn classifies_other_statuses() {
        let page = url("https://www.youtube.com/x");
        assert_eq!(
            check(StatusCode::BAD_GATEWAY, &page, None, String::new()).unwrap_err(),
            FetchError::Transient("HTTP 502 Bad Gateway".to_owned())
        );
        assert_eq!(
            check(StatusCode::NOT_FOUND, &page, None, "nope".to_owned()).unwrap_err(),
            FetchError::Unexpected {
                message: "HTTP 404 Not Found from /x".to_owned(),
                body: Some("nope".to_owned())
            }
        );
        assert_eq!(
            check(StatusCode::OK, &page, None, "fine".to_owned())
                .unwrap()
                .body,
            "fine"
        );
    }

    #[test]
    fn parses_retry_after_seconds() {
        assert_eq!(parse_retry_after(" 120 "), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }
}
