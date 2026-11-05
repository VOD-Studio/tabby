use std::{collections::HashMap, time::Duration};

use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    error::ApiError,
    models::{
        Configs, ProviderList, Proxies, ProxyProvider, Rule, RuleList, RuleProvider, Version,
    },
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

/// Upper bound for a request the kernel only answers once it has downloaded
/// something (geo databases, a new kernel build) from the internet.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

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

    /// Turns a non-success status into an [`ApiError`].
    async fn check(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(ApiError::from_status(status, &body))
    }

    /// `GET base/seg1/seg2/…`, decoded as JSON.
    async fn get_json<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T, ApiError> {
        self.get_json_at(self.url(segments)).await
    }

    /// Like [`Self::get_json`], with percent-encoded query parameters.
    async fn get_json_query<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let mut url = self.url(segments);
        for (index, (key, value)) in query.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(key);
            url.push('=');
            url.extend(utf8_percent_encode(value, PATH_SEGMENT));
        }
        self.get_json_at(url).await
    }

    async fn get_json_at<T: DeserializeOwned>(&self, url: String) -> Result<T, ApiError> {
        let response = Self::check(self.http.get(url).send().await?).await?;
        Ok(response.json().await?)
    }

    /// `PUT base/seg1/seg2/…` with a JSON body, ignoring the reply body.
    async fn put_json<B: Serialize + ?Sized>(
        &self,
        segments: &[&str],
        body: &B,
    ) -> Result<(), ApiError> {
        let request = self.http.put(self.url(segments)).json(body);
        Self::check(request.send().await?).await?;
        Ok(())
    }

    /// `PATCH base/seg1/seg2/…` with a JSON body, ignoring the reply body.
    async fn patch_json<B: Serialize + ?Sized>(
        &self,
        segments: &[&str],
        body: &B,
    ) -> Result<(), ApiError> {
        let request = self.http.patch(self.url(segments)).json(body);
        Self::check(request.send().await?).await?;
        Ok(())
    }

    /// `DELETE base/seg1/seg2/…`, ignoring the reply body.
    async fn delete(&self, segments: &[&str]) -> Result<(), ApiError> {
        Self::check(self.http.delete(self.url(segments)).send().await?).await?;
        Ok(())
    }

    /// `PUT base/seg1/seg2/…` without a body, ignoring the reply body.
    async fn put(&self, segments: &[&str]) -> Result<(), ApiError> {
        Self::check(self.http.put(self.url(segments)).send().await?).await?;
        Ok(())
    }

    /// `POST base/seg1/seg2/…` without a body, ignoring the reply body;
    /// the request may take up to `timeout`.
    async fn post(&self, segments: &[&str], timeout: Duration) -> Result<(), ApiError> {
        let request = self.http.post(self.url(segments)).timeout(timeout);
        Self::check(request.send().await?).await?;
        Ok(())
    }

    /// `GET /version`.
    pub async fn version(&self) -> Result<Version, ApiError> {
        self.get_json(&["version"]).await
    }

    /// `GET /configs`.
    pub async fn configs(&self) -> Result<Configs, ApiError> {
        self.get_json(&["configs"]).await
    }

    /// `PATCH /configs`: changes the running settings named in `body` (the
    /// mode, a port …). Replies 204.
    pub async fn patch_configs(&self, body: &serde_json::Value) -> Result<(), ApiError> {
        self.patch_json(&["configs"], body).await
    }

    /// `PUT /configs?force=true` with an empty path and payload: reloads
    /// the configuration file the kernel was started with. Replies 204.
    pub async fn reload_config(&self) -> Result<(), ApiError> {
        let url = format!("{}?force=true", self.url(&["configs"]));
        let body = serde_json::json!({ "path": "", "payload": "" });
        Self::check(self.http.put(url).json(&body).send().await?).await?;
        Ok(())
    }

    /// `POST /cache/fakeip/flush` (mihomo only). Replies 204, also when
    /// fake-ip is not in use.
    pub async fn flush_fake_ip(&self) -> Result<(), ApiError> {
        self.post(&["cache", "fakeip", "flush"], REQUEST_TIMEOUT)
            .await
    }

    /// `POST /configs/geo` (mihomo only): downloads the geo IP, ASN and site
    /// databases the configuration uses; mihomo v1.19.33 does not reload the
    /// configuration after it. The kernel replies once that is done, 204,
    /// so this may take long; with none in use it replies at once.
    pub async fn update_geo(&self) -> Result<(), ApiError> {
        self.post(&["configs", "geo"], DOWNLOAD_TIMEOUT).await
    }

    /// `POST /restart` (mihomo only). mihomo v1.19.33 replies 200
    /// `{"status":"ok"}` in full and only then replaces itself.
    pub async fn restart(&self) -> Result<(), ApiError> {
        self.post(&["restart"], REQUEST_TIMEOUT).await
    }

    /// `POST /upgrade` (mihomo only): downloads the newest kernel, replaces
    /// the running program with it and restarts. Fails with the kernel's
    /// message when there is nothing newer.
    pub async fn upgrade(&self) -> Result<(), ApiError> {
        self.post(&["upgrade"], DOWNLOAD_TIMEOUT).await
    }

    /// `GET /proxies`.
    pub async fn proxies(&self) -> Result<Proxies, ApiError> {
        self.get_json(&["proxies"]).await
    }

    /// `GET /rules`, in matching order.
    pub async fn rules(&self) -> Result<Vec<Rule>, ApiError> {
        let list: RuleList = self.get_json(&["rules"]).await?;
        Ok(list.rules)
    }

    /// `PUT /proxies/{group}`: makes a selector group route through `node`.
    pub async fn select_proxy(&self, group: &str, node: &str) -> Result<(), ApiError> {
        self.put_json(&["proxies", group], &serde_json::json!({ "name": node }))
            .await
    }

    /// `GET /proxies/{name}/delay`: milliseconds to fetch `url` through the
    /// node. A failed or timed-out test is an HTTP 503 / 504 error.
    pub async fn proxy_delay(
        &self,
        name: &str,
        url: &str,
        timeout_ms: u32,
    ) -> Result<u32, ApiError> {
        #[derive(Deserialize)]
        struct Reply {
            delay: u32,
        }
        let timeout = timeout_ms.to_string();
        let reply: Reply = self
            .get_json_query(
                &["proxies", name, "delay"],
                &[("url", url), ("timeout", &timeout)],
            )
            .await?;
        Ok(reply.delay)
    }

    /// `GET /group/{name}/delay` (mihomo only): delays of the members that
    /// answered; the ones that failed are left out.
    pub async fn group_delay(
        &self,
        group: &str,
        url: &str,
        timeout_ms: u32,
    ) -> Result<HashMap<String, u32>, ApiError> {
        let timeout = timeout_ms.to_string();
        self.get_json_query(
            &["group", group, "delay"],
            &[("url", url), ("timeout", &timeout)],
        )
        .await
    }

    /// `DELETE /connections/{id}`: closes one connection. Replies 204; mihomo
    /// does so for an id it does not know, too.
    pub async fn close_connection(&self, id: &str) -> Result<(), ApiError> {
        self.delete(&["connections", id]).await
    }

    /// `DELETE /connections`: closes every connection. Replies 204.
    pub async fn close_all_connections(&self) -> Result<(), ApiError> {
        self.delete(&["connections"]).await
    }

    /// `GET /providers/proxies`: the proxy providers that can be updated,
    /// by name.
    pub async fn proxy_providers(&self) -> Result<Vec<ProxyProvider>, ApiError> {
        let list: ProviderList<ProxyProvider> = self.get_json(&["providers", "proxies"]).await?;
        Ok(list.shown())
    }

    /// `GET /providers/rules` (mihomo only): the rule providers, by name.
    pub async fn rule_providers(&self) -> Result<Vec<RuleProvider>, ApiError> {
        let list: ProviderList<RuleProvider> = self.get_json(&["providers", "rules"]).await?;
        Ok(list.shown())
    }

    /// `PUT /providers/proxies/{name}`: reloads a proxy provider from its
    /// source (a file or inline one too). Replies 204.
    pub async fn update_proxy_provider(&self, name: &str) -> Result<(), ApiError> {
        self.put(&["providers", "proxies", name]).await
    }

    /// `PUT /providers/rules/{name}` (mihomo only). Replies 204.
    pub async fn update_rule_provider(&self, name: &str) -> Result<(), ApiError> {
        self.put(&["providers", "rules", name]).await
    }

    /// `GET /providers/proxies/{name}/healthcheck`: tests every node of the
    /// provider. The kernel replies 204 only once all of them are done, a
    /// dead one after the provider's own test timeout, so this request gets
    /// `timeout` instead of the usual limit.
    pub async fn healthcheck_proxy_provider(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<(), ApiError> {
        let request = self
            .http
            .get(self.url(&["providers", "proxies", name, "healthcheck"]))
            .timeout(timeout);
        Self::check(request.send().await?).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path, query_param},
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
    async fn select_proxy_puts_name_in_body_and_encodes_group() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path(
                "/proxies/%E8%8A%82%E7%82%B9%2F%E9%80%89%E6%8B%A9%20%231",
            ))
            .and(body_json(json!({"name": "🚀 a/b#c"})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = ApiClient::new(&server.uri(), None).unwrap();
        client
            .select_proxy("节点/选择 #1", "🚀 a/b#c")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn closing_deletes_one_encoded_id_or_every_connection() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/connections/a%2Fb%20c%3F"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/connections"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = ApiClient::new(&server.uri(), None).unwrap();
        client.close_connection("a/b c?").await.unwrap();
        client.close_all_connections().await.unwrap();
        // Anything else is unmatched, which wiremock answers with 404.
        assert!(matches!(
            client.close_connection("gone").await,
            Err(ApiError::NotFound)
        ));
    }

    #[tokio::test]
    async fn provider_actions_use_their_methods_and_encoded_names() {
        const PROXY: &str = "/providers/proxies/%E6%9C%BA%E5%9C%BA%20%E8%AE%A2%E9%98%85%2F1";
        const RULE: &str = "/providers/rules/%E6%B5%8B%E8%AF%95%20%E5%9F%9F%E5%90%8D%232";
        let server = MockServer::start().await;
        for (verb, route) in [
            ("PUT", PROXY.to_owned()),
            ("PUT", RULE.to_owned()),
            ("GET", format!("{PROXY}/healthcheck")),
        ] {
            Mock::given(method(verb))
                .and(path(route))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }

        let client = ApiClient::new(&server.uri(), None).unwrap();
        client.update_proxy_provider("机场 订阅/1").await.unwrap();
        client.update_rule_provider("测试 域名#2").await.unwrap();
        client
            .healthcheck_proxy_provider("机场 订阅/1", Duration::from_secs(5))
            .await
            .unwrap();
        // The other kind's endpoint is not the same one.
        assert!(matches!(
            client.update_rule_provider("机场 订阅/1").await,
            Err(ApiError::NotFound)
        ));
    }

    #[tokio::test]
    async fn proxy_delay_encodes_name_and_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/proxies/a%2Fb%23c%25d%20e%3Ff/delay"))
            .and(query_param("url", "https://t.example/generate_204?x=1&y=2"))
            .and(query_param("timeout", "5000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"delay": 42})))
            .expect(1)
            .mount(&server)
            .await;

        let client = ApiClient::new(&server.uri(), None).unwrap();
        let delay = client
            .proxy_delay(
                "a/b#c%d e?f",
                "https://t.example/generate_204?x=1&y=2",
                5000,
            )
            .await
            .unwrap();
        assert_eq!(delay, 42);
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
