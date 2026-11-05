use std::time::Duration;

use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;

use super::{
    error::ApiError,
    models::{Configs, Proxies, Version},
};

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

    /// `GET base/seg1/seg2/…`, decoded as JSON.
    async fn get_json<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T, ApiError> {
        let response = self.http.get(self.url(segments)).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::from_status(status, &body));
        }
        Ok(response.json().await?)
    }

    /// `GET /version`.
    pub async fn version(&self) -> Result<Version, ApiError> {
        self.get_json(&["version"]).await
    }

    /// `GET /configs`.
    pub async fn configs(&self) -> Result<Configs, ApiError> {
        self.get_json(&["configs"]).await
    }

    /// `GET /proxies`.
    pub async fn proxies(&self) -> Result<Proxies, ApiError> {
        self.get_json(&["proxies"]).await
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;

    const VERSION_JSON: &str = include_str!("../../tests/fixtures/mihomo_version.json");

    fn version_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(VERSION_JSON, "application/json")
    }

    #[tokio::test]
    async fn sends_secret_as_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(version_response())
            .expect(1)
            .mount(&server)
            .await;

        let client = ApiClient::new(&server.uri(), Some("s3cret")).unwrap();
        assert_eq!(client.version().await.unwrap().version, "v1.19.33");
    }

    #[tokio::test]
    async fn omits_authorization_without_secret() {
        let server = MockServer::start().await;
        Mock::given(path("/version"))
            .respond_with(version_response())
            .mount(&server)
            .await;

        ApiClient::new(&server.uri(), None)
            .unwrap()
            .version()
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(requests[0].headers.get("authorization").is_none());
    }

    #[tokio::test]
    async fn encodes_path_segments() {
        let server = MockServer::start().await;
        // One segment containing every character that could split or alter a path.
        Mock::given(method("GET"))
            .and(path(
                "/proxies/a%2Fb%23c%25d%20e%3Ff%E4%B8%AD%F0%9F%98%80/delay",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"delay": 42})))
            .expect(1)
            .mount(&server)
            .await;

        // A trailing slash on the base URL must not produce `//` either.
        let client = ApiClient::new(&format!("{}/", server.uri()), None).unwrap();
        let reply: Value = client
            .get_json(&["proxies", "a/b#c%d e?f中😀", "delay"])
            .await
            .unwrap();
        assert_eq!(reply["delay"], 42);
    }

    #[tokio::test]
    async fn maps_error_statuses() {
        let server = MockServer::start().await;
        for (route, response) in [
            (
                "/unauthorized",
                ResponseTemplate::new(401).set_body_json(json!({"message": "Unauthorized"})),
            ),
            (
                "/missing",
                ResponseTemplate::new(404).set_body_string("404 page not found"),
            ),
            (
                "/json-error",
                ResponseTemplate::new(400).set_body_json(json!({"message": "Body invalid"})),
            ),
            (
                "/server-error",
                ResponseTemplate::new(503).set_body_string("upstream down"),
            ),
            ("/no-body", ResponseTemplate::new(500)),
            (
                "/not-json",
                ResponseTemplate::new(200).set_body_string("<html>"),
            ),
        ] {
            Mock::given(path(route))
                .respond_with(response)
                .mount(&server)
                .await;
        }
        let client = ApiClient::new(&server.uri(), None).unwrap();
        let get = |route: &'static str| {
            let client = client.clone();
            async move { client.get_json::<Value>(&[route]).await.unwrap_err() }
        };

        assert!(matches!(get("unauthorized").await, ApiError::Unauthorized));
        assert!(matches!(get("missing").await, ApiError::NotFound));
        assert!(matches!(
            get("json-error").await,
            ApiError::Http { status: 400, message } if message == "Body invalid"
        ));
        assert!(matches!(
            get("server-error").await,
            ApiError::Http { status: 503, message } if message == "upstream down"
        ));
        assert!(matches!(
            get("no-body").await,
            ApiError::Http { status: 500, message } if message == "Internal Server Error"
        ));
        assert!(matches!(get("not-json").await, ApiError::Decode(_)));
    }

    /// Re-runs `proxy_env_child` in a subprocess with proxy variables pointing at
    /// a second server: environment variables cannot be set in-process
    /// (`unsafe_code` is forbidden), and the client must ignore them (R1).
    #[tokio::test]
    async fn ignores_proxy_environment() {
        let controller = MockServer::start().await;
        Mock::given(path("/version"))
            .respond_with(version_response())
            .mount(&controller)
            .await;
        let proxy = MockServer::start().await;

        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--ignored",
                "--exact",
                "api::client::tests::proxy_env_child",
            ])
            .env("TABBY_TEST_CONTROLLER", controller.uri())
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for var in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            child.env(var, proxy.uri());
        }
        let output = child.output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(controller.received_requests().await.unwrap().len(), 1);
        assert!(proxy.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    #[ignore = "subprocess body of ignores_proxy_environment"]
    async fn proxy_env_child() {
        let url = std::env::var("TABBY_TEST_CONTROLLER").unwrap();
        ApiClient::new(&url, None).unwrap().version().await.unwrap();
    }
}
