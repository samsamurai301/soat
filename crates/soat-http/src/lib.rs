//! An outbound HTTPS client for adapters to outside services (payment
//! providers, carriers): hyper with rustls and the Mozilla roots, plain
//! HTTP allowed for local test doubles, a timeout on every call.
//!
//! Zapline follow-up: Zapline has the same client inside (S3, OAuth);
//! once it is public there, this goes.

use std::sync::OnceLock;
use std::time::Duration;

pub use bytes::Bytes;
pub use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
pub use http_body_util::Full;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use zapline::prelude::*;

const TIMEOUT: Duration = Duration::from_secs(20);

type HttpsClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

fn client() -> &'static HttpsClient {
    static CLIENT: OnceLock<HttpsClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Client::builder(TokioExecutor::new()).build(https)
    })
}

/// A buffered answer.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: StatusCode,
    pub body: Bytes,
}

impl Answer {
    /// The body as JSON; `Null` when it is empty or not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Sends `req`. No answer in time, or no connection, is a 503 naming
/// `service` (e.g. `Mollie`).
///
/// # Errors
///
/// `503` when the service cannot be reached.
pub async fn send(service: &str, req: Request<Full<Bytes>>) -> Result<Answer> {
    let unavailable = |e: String| Error::service_unavailable(format!("{service}: {e}"));
    let res = tokio::time::timeout(TIMEOUT, client().request(req))
        .await
        .map_err(|_| unavailable("timed out".into()))?
        .map_err(|e| unavailable(e.to_string()))?;
    let status = res.status();
    let body = tokio::time::timeout(TIMEOUT, res.into_body().collect())
        .await
        .map_err(|_| unavailable("timed out".into()))?
        .map_err(|e| unavailable(e.to_string()))?
        .to_bytes();
    Ok(Answer { status, body })
}

pub use soat_webhooks::TargetPolicy;

/// What [`fetch`] got.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The `content-type` it was served with, if any.
    pub content_type: Option<String>,
    pub body: Bytes,
}

/// Resolves names to their public addresses only, so a name cannot point
/// the server at its own network, also when it changes between a check
/// and the connection.
#[derive(Clone)]
struct PublicOnly;

impl tower_service::Service<hyper_util::client::legacy::connect::dns::Name> for PublicOnly {
    type Response = std::vec::IntoIter<std::net::SocketAddr>;
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Self::Response>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: hyper_util::client::legacy::connect::dns::Name) -> Self::Future {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let all: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<_> = all
                .into_iter()
                .filter(|a| soat_webhooks::is_public(a.ip()))
                .collect();
            if public.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{host} has no public address"),
                ));
            }
            Ok(public.into_iter())
        })
    }
}

type PublicClient = Client<HttpsConnector<HttpConnector<PublicOnly>>, Full<Bytes>>;

fn public_client() -> &'static PublicClient {
    static CLIENT: OnceLock<PublicClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut http = HttpConnector::new_with_resolver(PublicOnly);
        http.enforce_http(false);
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_only()
            .enable_http1()
            .wrap_connector(http);
        Client::builder(TokioExecutor::new()).build(https)
    })
}

/// Most redirects [`fetch`] follows.
const REDIRECTS: usize = 3;

/// GETs `url`, a link a shop gave, for its bytes: with
/// [`TargetPolicy::PublicHttps`] only `https://` to public addresses
/// (checked as the connection is made, and again on each of up to three
/// redirects), at most `max_bytes`.
///
/// # Errors
///
/// 422 `invalid_url` for a URL the policy refuses, 422 `fetch_failed` when
/// it answers other than 2xx or cannot be reached, 413 `too_large` past
/// `max_bytes`.
pub async fn fetch(url: &str, policy: TargetPolicy, max_bytes: usize) -> Result<Fetched> {
    let failed = |why: String| Error::unprocessable(why).with_code("fetch_failed");
    let mut url = url.trim().to_owned();
    for _ in 0..=REDIRECTS {
        let uri = soat_webhooks::check_target(&url, policy).map_err(|why| {
            Error::unprocessable(format!("{url}: {why}")).with_code("invalid_url")
        })?;
        let req = Request::get(uri.clone())
            .header("user-agent", "SOAT/1")
            .header("accept", "image/*, */*;q=0.5")
            .body(Full::new(Bytes::new()))
            .map_err(|e| failed(e.to_string()))?;
        let sent = match policy {
            TargetPolicy::PublicHttps => public_client().request(req),
            TargetPolicy::Any => client().request(req),
        };
        let res = tokio::time::timeout(TIMEOUT, sent)
            .await
            .map_err(|_| failed(format!("{url}: timed out")))?
            .map_err(|e| {
                let mut why = e.to_string();
                let mut source = std::error::Error::source(&e);
                while let Some(s) = source {
                    why = s.to_string();
                    source = s.source();
                }
                failed(format!("{url}: {why}"))
            })?;
        let status = res.status();
        if status.is_redirection() {
            let Some(next) = res
                .headers()
                .get(http::header::LOCATION)
                .and_then(|l| l.to_str().ok())
            else {
                return Err(failed(format!("{url}: a redirect without a location")));
            };
            url = if next.starts_with("http://") || next.starts_with("https://") {
                next.to_owned()
            } else if next.starts_with('/') && !next.starts_with("//") {
                format!(
                    "{}://{}{next}",
                    uri.scheme_str().unwrap_or("https"),
                    uri.authority().map(|a| a.as_str()).unwrap_or_default()
                )
            } else {
                return Err(failed(format!("{url}: a redirect to {next}")));
            };
            continue;
        }
        if !status.is_success() {
            return Err(failed(format!("{url}: answered {}", status.as_u16())));
        }
        let too_large = || {
            Error::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("{url}: larger than {} MB", max_bytes / (1024 * 1024)),
            )
            .with_code("too_large")
        };
        let content_type = res
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let declared = res
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        if declared.is_some_and(|n| n > max_bytes) {
            return Err(too_large());
        }
        let mut body = res.into_body();
        let mut out = Vec::new();
        let read = async {
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|e| failed(format!("{url}: {e}")))?;
                if let Ok(data) = frame.into_data() {
                    if out.len() + data.len() > max_bytes {
                        return Err(too_large());
                    }
                    out.extend_from_slice(&data);
                }
            }
            Ok(())
        };
        tokio::time::timeout(TIMEOUT, read)
            .await
            .map_err(|_| failed(format!("{url}: timed out")))??;
        return Ok(Fetched {
            content_type,
            body: Bytes::from(out),
        });
    }
    Err(failed(format!("{url}: more than {REDIRECTS} redirects")))
}

/// `application/x-www-form-urlencoded` from pairs.
pub fn form(pairs: &[(&str, String)]) -> String {
    fn enc(s: &str, out: &mut String) {
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        enc(k, &mut out);
        out.push('=');
        enc(v, &mut out);
    }
    out
}

/// Reads one field of a form-encoded body, e.g. Mollie's `id=tr_...`.
pub fn form_field(body: &[u8], name: &str) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    text.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (decode(k)? == name).then(|| decode(v)).flatten()
    })
}

fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        let body = form(&[("metadata[a b]", "x&y=é".into()), ("n", "1".into())]);
        assert_eq!(body, "metadata%5Ba%20b%5D=x%26y%3D%C3%A9&n=1");
        assert_eq!(
            form_field(body.as_bytes(), "metadata[a b]").as_deref(),
            Some("x&y=é")
        );
        assert_eq!(
            form_field(b"id=tr_12+3&x=1", "id").as_deref(),
            Some("tr_12 3")
        );
        assert_eq!(form_field(b"id=%zz", "id"), None);
        assert_eq!(form_field(b"x=1", "id"), None);
    }
}
