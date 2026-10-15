//! HTTP fetching with SSRF protection, conditional GET and size limits.
//!
//! Feed URLs come from configuration and, later, from content itself (links
//! in newsletters), so every request is treated as attacker-influenced:
//!
//! * DNS answers are filtered by a custom resolver: only public unicast
//!   addresses are returned to the connector. Because the connector uses
//!   exactly these addresses, there is no window for DNS rebinding between
//!   the check and the connection.
//! * IP-literal hosts never reach the resolver, so they are checked
//!   explicitly, on the first URL and on every redirect hop.
//! * Redirects: at most 5, http(s) only, each hop re-validated.
//! * Bodies are read in chunks and abandoned past `max_bytes`.
//! * No proxy is read from the environment, no cookies are kept.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use reqwest::{StatusCode, redirect};
use url::Url;

const USER_AGENT: &str = concat!(
    "SignalSift/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/v1cferr/signal-sift)"
);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("refused URL {0}: {1}")]
    Refused(String, &'static str),
    #[error("HTTP {status} from {url}")]
    Status { url: String, status: u16 },
    #[error("response from {url} exceeds {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("request to {url} failed: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
}

/// Cache validators from a previous response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub enum Fetched {
    NotModified,
    Body {
        final_url: Url,
        status: u16,
        body: Vec<u8>,
        validators: Validators,
    },
}

#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    max_bytes: u64,
    allow_private: bool,
}

impl Fetcher {
    pub fn new(timeout: Duration, max_bytes: u64) -> Result<Self, reqwest::Error> {
        Self::build(timeout, max_bytes, false)
    }

    /// Only for tests against a local server.
    #[cfg(test)]
    pub(crate) fn allowing_private(timeout: Duration, max_bytes: u64) -> Self {
        Self::build(timeout, max_bytes, true).unwrap()
    }

    fn build(
        timeout: Duration,
        max_bytes: u64,
        allow_private: bool,
    ) -> Result<Self, reqwest::Error> {
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .no_proxy()
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                match check_url(attempt.url(), allow_private) {
                    Ok(()) => attempt.follow(),
                    Err(reason) => attempt.error(reason),
                }
            }))
            // Idle keep-alive connections cost memory and file descriptors in
            // a daemon that fetches a few feeds per hour; close them quickly.
            .pool_idle_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(1);
        if !allow_private {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        Ok(Self {
            client: builder.build()?,
            max_bytes,
            allow_private,
        })
    }

    /// GET with conditional headers. A 304 yields [`Fetched::NotModified`].
    pub async fn get(&self, url: &Url, validators: &Validators) -> Result<Fetched, FetchError> {
        check_url(url, self.allow_private).map_err(|r| FetchError::Refused(url.to_string(), r))?;
        let mut req = self.client.get(url.clone());
        if let Some(etag) = &validators.etag {
            req = req.header(IF_NONE_MATCH, etag);
        }
        if let Some(lm) = &validators.last_modified {
            req = req.header(IF_MODIFIED_SINCE, lm);
        }
        let request_err = |source| FetchError::Request {
            url: url.to_string(),
            source,
        };
        let mut res = req.send().await.map_err(request_err)?;
        let status = res.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(Fetched::NotModified);
        }
        if !status.is_success() {
            return Err(FetchError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let too_large = || FetchError::TooLarge {
            url: url.to_string(),
            limit: self.max_bytes,
        };
        if res.content_length().is_some_and(|n| n > self.max_bytes) {
            return Err(too_large());
        }
        let header = |name| {
            res.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_owned)
        };
        let validators = Validators {
            etag: header(ETAG),
            last_modified: header(LAST_MODIFIED),
        };
        let final_url = res.url().clone();
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(request_err)? {
            if body.len() as u64 + chunk.len() as u64 > self.max_bytes {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Fetched::Body {
            final_url,
            status: status.as_u16(),
            body,
            validators,
        })
    }
}

fn check_url(url: &Url, allow_private: bool) -> Result<(), &'static str> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https are allowed");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in URLs are not allowed");
    }
    match url.host() {
        None => Err("URL has no host"),
        Some(url::Host::Ipv4(ip)) if !allow_private && !is_public(IpAddr::V4(ip)) => {
            Err("address is not public")
        }
        Some(url::Host::Ipv6(ip)) if !allow_private && !is_public(IpAddr::V6(ip)) => {
            Err("address is not public")
        }
        Some(_) => Ok(()),
    }
}

struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} resolves to no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// True for globally routable unicast addresses. Everything a request could
/// use to reach this machine, the LAN, cloud metadata services or special
/// infrastructure is excluded.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg = v6.segments();
            // NAT64 (64:ff9b::/96) embeds an IPv4 address.
            if seg[0..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = seg[6].to_be_bytes();
                let [c, d] = seg[7].to_be_bytes();
                return is_public_v4(Ipv4Addr::new(a, b, c, d));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link local fe80::/10
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || v6 == Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1))
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local() // 169.254/16, cloud metadata
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b)) // CGNAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || a >= 240) // reserved
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn classifies_addresses() {
        for public in ["1.1.1.1", "185.199.108.153", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
    }

    #[test]
    fn refuses_bad_urls_before_any_network_io() {
        for url in [
            "http://127.0.0.1/feed",
            "http://[::1]/feed",
            "http://169.254.169.254/latest/meta-data/",
            "file:///etc/passwd",
            "ftp://example.com/feed",
            "https://user:pass@example.com/feed",
        ] {
            assert!(
                check_url(&Url::parse(url).unwrap(), false).is_err(),
                "{url}"
            );
        }
        assert!(
            check_url(
                &Url::parse("https://akitaonrails.com/index.xml").unwrap(),
                false
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn hostnames_resolving_to_loopback_are_refused() {
        let fetcher = Fetcher::new(Duration::from_secs(5), 1024).unwrap();
        let err = fetcher
            .get(
                &Url::parse("http://localhost:9/").unwrap(),
                &Validators::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Request { .. }), "{err:?}");
        assert!(
            format!("{:?}", err).contains("no public address"),
            "{err:?}"
        );
    }

    /// Minimal HTTP/1.1 server: answers 304 when If-None-Match matches.
    async fn serve(body: &'static str, etag: &'static str) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let resp = if req.contains(&format!("if-none-match: {etag}")) {
                    "HTTP/1.1 304 Not Modified\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\netag: {etag}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        Url::parse(&format!("http://{addr}/feed.xml")).unwrap()
    }

    #[tokio::test]
    async fn conditional_get_round_trip() {
        let url = serve("<rss/>", "\"v1\"").await;
        let f = Fetcher::allowing_private(Duration::from_secs(5), 1024);
        let Fetched::Body {
            body, validators, ..
        } = f.get(&url, &Validators::default()).await.unwrap()
        else {
            panic!("expected a body");
        };
        assert_eq!(body, b"<rss/>");
        assert_eq!(validators.etag.as_deref(), Some("\"v1\""));
        assert!(matches!(
            f.get(&url, &validators).await.unwrap(),
            Fetched::NotModified
        ));
    }

    #[tokio::test]
    async fn oversized_bodies_are_abandoned() {
        let url = serve("0123456789abcdef", "\"v\"").await;
        let f = Fetcher::allowing_private(Duration::from_secs(5), 8);
        let err = f.get(&url, &Validators::default()).await.unwrap_err();
        assert!(matches!(err, FetchError::TooLarge { .. }));
    }
}
