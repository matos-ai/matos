use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use reqwest::Url;

use crate::{credentials::Credentials, error::ProxyError, forward, proxy::Proxy};

const MAX_REDIRECTS: usize = 3;
const MAX_PATH_BYTES: usize = 1024;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const KEPT_REQUEST: [HeaderName; 5] = [
    header::ACCEPT,
    header::USER_AGENT,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::RANGE,
];
const KEPT_RESPONSE: [HeaderName; 8] = [
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_ENCODING,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
    header::ETAG,
    header::LAST_MODIFIED,
    header::CACHE_CONTROL,
];

#[derive(Debug, Clone)]
pub struct RegistryConfig {
    pub crates_index_base: String,
    pub crates_static_base: String,
    pub npm_base: String,
    pub public_base: Option<String>,
}

impl RegistryConfig {
    pub fn trimmed(self) -> Self {
        let trim = |base: String| base.trim_end_matches('/').to_owned();
        Self {
            crates_index_base: trim(self.crates_index_base),
            crates_static_base: trim(self.crates_static_base),
            npm_base: trim(self.npm_base),
            public_base: self.public_base.map(trim).filter(|base| !base.is_empty()),
        }
    }

    fn origins(&self) -> Vec<Url> {
        [
            &self.crates_index_base,
            &self.crates_static_base,
            &self.npm_base,
        ]
        .into_iter()
        .filter_map(|base| Url::parse(base).ok())
        .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registry {
    CratesIndex,
    Crates,
    Npm,
}

impl Registry {
    fn prefix(self) -> &'static str {
        match self {
            Self::CratesIndex => "/crates-index/",
            Self::Crates => "/crates/",
            Self::Npm => "/npm/",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::CratesIndex => "registry:crates-index",
            Self::Crates => "registry:crates",
            Self::Npm => "registry:npm",
        }
    }

    fn base(self, config: &RegistryConfig) -> &str {
        match self {
            Self::CratesIndex => &config.crates_index_base,
            Self::Crates => &config.crates_static_base,
            Self::Npm => &config.npm_base,
        }
    }
}

pub async fn crates_index<C: Credentials>(
    State(proxy): State<Arc<Proxy<C>>>,
    method: Method,
    Path(path): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    serve(&proxy, Registry::CratesIndex, method, &path, &uri, &headers).await
}

pub async fn crates<C: Credentials>(
    State(proxy): State<Arc<Proxy<C>>>,
    method: Method,
    Path(path): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    serve(&proxy, Registry::Crates, method, &path, &uri, &headers).await
}

pub async fn npm<C: Credentials>(
    State(proxy): State<Arc<Proxy<C>>>,
    method: Method,
    Path(path): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    serve(&proxy, Registry::Npm, method, &path, &uri, &headers).await
}

async fn serve<C: Credentials>(
    proxy: &Proxy<C>,
    registry: Registry,
    method: Method,
    path: &str,
    uri: &Uri,
    headers: &HeaderMap,
) -> Response {
    match fetch(proxy, registry, &method, path, uri, headers).await {
        Ok(response) => response,
        Err(Failure::Refused(reason)) => (StatusCode::BAD_REQUEST, reason).into_response(),
        Err(Failure::Unavailable(reason)) => {
            (StatusCode::SERVICE_UNAVAILABLE, reason).into_response()
        }
        Err(Failure::Proxy(error)) => {
            error.log(&method, registry.class());
            error.into_response()
        }
    }
}

enum Failure {
    Refused(&'static str),
    Unavailable(&'static str),
    Proxy(ProxyError),
}

impl From<ProxyError> for Failure {
    fn from(error: ProxyError) -> Self {
        Self::Proxy(error)
    }
}

fn valid_path(decoded: &str) -> bool {
    !decoded.is_empty()
        && decoded.len() <= MAX_PATH_BYTES
        && !decoded.chars().any(|c| c.is_control() || c == '\\')
        && decoded
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

async fn fetch<C: Credentials>(
    proxy: &Proxy<C>,
    registry: Registry,
    method: &Method,
    decoded: &str,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<Response, Failure> {
    if !valid_path(decoded) {
        return Err(Failure::Refused("the path is not allowed"));
    }
    let config = &proxy.config.registries;
    let raw = uri
        .path()
        .strip_prefix(registry.prefix())
        .ok_or(Failure::Refused("the path is not allowed"))?;
    let rewrites_config = registry == Registry::CratesIndex && decoded == "config.json";
    if rewrites_config && config.public_base.is_none() {
        return Err(Failure::Unavailable("the registry is not configured"));
    }
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let target = Url::parse(&format!("{}/{raw}{query}", registry.base(config)))
        .map_err(|_| Failure::Refused("the path is not allowed"))?;
    let upstream = send(
        proxy,
        method.clone(),
        target,
        forward::copy_allowed(headers, &KEPT_REQUEST),
        &config.origins(),
    )
    .await?;
    if rewrites_config && upstream.status().is_success() {
        let public = config.public_base.as_deref().unwrap_or_default();
        return Ok(rewritten_config(upstream, public).await?);
    }
    Ok(forward::relay_bounded(upstream, &KEPT_RESPONSE))
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

async fn send<C: Credentials>(
    proxy: &Proxy<C>,
    method: Method,
    mut url: Url,
    headers: HeaderMap,
    origins: &[Url],
) -> Result<reqwest::Response, ProxyError> {
    for _ in 0..=MAX_REDIRECTS {
        let response = proxy
            .http
            .request(method.clone(), url.clone())
            .headers(headers.clone())
            .send()
            .await
            .map_err(|error| ProxyError::Upstream(error.without_url().to_string()))?;
        let redirected = matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308);
        let Some(location) = redirected
            .then(|| response.headers().get(header::LOCATION))
            .flatten()
        else {
            return Ok(response);
        };
        let next = location
            .to_str()
            .ok()
            .and_then(|location| url.join(location).ok())
            .ok_or_else(|| ProxyError::Upstream("an unusable redirect".into()))?;
        if !origins.iter().any(|origin| same_origin(origin, &next)) {
            return Err(ProxyError::Upstream(
                "a redirect left the registry allowlist".into(),
            ));
        }
        url = next;
    }
    Err(ProxyError::Upstream("too many redirects".into()))
}

async fn rewritten_config(
    upstream: reqwest::Response,
    public: &str,
) -> Result<Response, ProxyError> {
    if upstream
        .content_length()
        .is_some_and(|length| length > MAX_CONFIG_BYTES as u64)
    {
        return Err(ProxyError::Upstream(
            "the index configuration is too large".into(),
        ));
    }
    let bytes = upstream
        .bytes()
        .await
        .map_err(|error| ProxyError::Upstream(error.without_url().to_string()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ProxyError::Upstream(
            "the index configuration is too large".into(),
        ));
    }
    let mut config: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| ProxyError::Upstream("the index configuration is unreadable".into()))?;
    let object = config
        .as_object_mut()
        .ok_or_else(|| ProxyError::Upstream("the index configuration is unreadable".into()))?;
    object.insert(
        "dl".to_owned(),
        serde_json::Value::String(format!("{public}/crates")),
    );
    object.remove("api");
    let body = serde_json::to_vec(&config)
        .map_err(|_| ProxyError::Upstream("the index configuration is unusable".into()))?;
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_may_not_climb_or_hide_empty_segments() {
        for good in [
            "config.json",
            "se/rd/serde",
            "@babel/core",
            "left-pad",
            "x/-/x-1.0.0.tgz",
        ] {
            assert!(valid_path(good), "{good}");
        }
        for bad in [
            "", "..", "a/../b", "./a", "a//b", "a/", "/a", "a\\b", "a\nb", "a/.",
        ] {
            assert!(!valid_path(bad), "{bad:?}");
        }
        assert!(!valid_path(&"a".repeat(MAX_PATH_BYTES + 1)));
    }

    #[test]
    fn origins_are_compared_by_scheme_host_and_port() {
        let a = Url::parse("https://static.crates.io/crates").unwrap();
        assert!(same_origin(
            &a,
            &Url::parse("https://static.crates.io/x").unwrap()
        ));
        assert!(same_origin(
            &a,
            &Url::parse("https://static.crates.io:443/x").unwrap()
        ));
        assert!(!same_origin(
            &a,
            &Url::parse("http://static.crates.io/x").unwrap()
        ));
        assert!(!same_origin(
            &a,
            &Url::parse("https://evil.example/x").unwrap()
        ));
        assert!(!same_origin(
            &a,
            &Url::parse("https://static.crates.io:8443/x").unwrap()
        ));
    }

    #[test]
    fn the_configured_bases_lose_their_trailing_slash() {
        let config = RegistryConfig {
            crates_index_base: "https://index.crates.io/".into(),
            crates_static_base: "https://static.crates.io/crates//".into(),
            npm_base: "https://registry.npmjs.org".into(),
            public_base: Some("http://proxy:4130/".into()),
        }
        .trimmed();
        assert_eq!(config.crates_index_base, "https://index.crates.io");
        assert_eq!(config.crates_static_base, "https://static.crates.io/crates");
        assert_eq!(config.public_base.as_deref(), Some("http://proxy:4130"));
        let empty = RegistryConfig {
            public_base: Some(String::new()),
            ..config
        }
        .trimmed();
        assert_eq!(empty.public_base, None);
    }
}
