use std::time::Duration;

use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;

/// Everything except RFC 3986 unreserved characters is escaped, so a name can
/// never inject `/`, `?`, `#` or `%` into the request path.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound for one request; longer than the default 5 s delay-test timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Client for the Clash / mihomo External Controller REST API.
#[derive(Debug, Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    base: String,
}

impl ApiClient {
    /// Builds a client for the controller at `base_url`.
    ///
    /// The secret is sent as `Authorization: Bearer <secret>` on every request.
    /// Proxies are disabled explicitly: with `HTTP_PROXY` set, requests would
    /// otherwise loop back through the very Clash instance being controlled.
    pub fn new(base_url: &str, secret: Option<&str>) -> Result<Self> {
        let url = reqwest::Url::parse(base_url)
            .wrap_err_with(|| format!("invalid controller url `{base_url}`"))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("controller url `{base_url}` must use http or https");
        }

        let mut headers = HeaderMap::new();
        if let Some(secret) = secret {
            let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
                .wrap_err("secret contains characters that cannot be sent in an HTTP header")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }

        let http = reqwest::Client::builder()
            .no_proxy()
            .default_headers(headers)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .wrap_err("cannot build HTTP client")?;
        Ok(Self {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// `base/seg1/seg2/…`, with every segment percent-encoded.
    fn url(&self, segments: &[&str]) -> String {
        let mut url = self.base.clone();
        for segment in segments {
            url.push('/');
            url.extend(utf8_percent_encode(segment, PATH_SEGMENT));
        }
        url
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        segments: &[&str],
    ) -> Result<T, reqwest::Error> {
        self.http
            .get(self.url(segments))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }
}
