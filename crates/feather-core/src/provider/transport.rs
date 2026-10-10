//! Provider HTTP transport routing.
//!
//! Defaults are deliberately direct (`no_proxy`) so provider requests do not
//! inherit process or OS proxy settings unless the caller explicitly installs a
//! [`Route::Proxy`]. Discovery only probes loopback ports.

use super::{EventHandler, ProviderError, ResponseStream};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;
use thiserror::Error;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(150);
const PROBE_IO_TIMEOUT: Duration = Duration::from_millis(200);
const DISCOVERY_PORTS: [u16; 10] = [12334, 7890, 7891, 7897, 1080, 10808, 10809, 8080, 8118, 8888];

/// Process-local provider transport route.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Route {
    /// Direct sockets only; disables reqwest environment/system proxy lookup.
    Direct,
    /// Explicit proxy URL, canonicalized as `http://host:port` or
    /// `socks5h://host:port`. Credentials, paths, queries, and fragments are
    /// rejected so catalog rows never carry secrets.
    Proxy { url: String },
}

impl Default for Route {
    fn default() -> Self {
        Self::Direct
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Direct => f.write_str("direct"),
            Route::Proxy { url } => f.write_str(url),
        }
    }
}

impl FromStr for Route {
    type Err = TransportError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("direct") {
            Ok(Self::Direct)
        } else {
            Self::proxy(s)
        }
    }
}

impl Route {
    pub fn direct() -> Self {
        Self::Direct
    }

    pub fn proxy(url: impl AsRef<str>) -> Result<Self, TransportError> {
        Ok(Self::Proxy {
            url: canonical_proxy_url(url.as_ref())?,
        })
    }

    pub fn proxy_url(&self) -> Option<&str> {
        match self {
            Route::Direct => None,
            Route::Proxy { url } => Some(url.as_str()),
        }
    }

    /// Build an async reqwest client builder for this route.
    pub fn async_builder(&self) -> reqwest::ClientBuilder {
        let builder = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT);
        match self {
            Route::Direct => builder,
            Route::Proxy { url } => builder.proxy(
                reqwest::Proxy::all(url).expect("validated proxy URL must be accepted by reqwest"),
            ),
        }
    }

    /// Build a blocking reqwest client builder for this route.
    pub fn blocking_builder(&self) -> reqwest::blocking::ClientBuilder {
        let builder = reqwest::blocking::Client::builder()
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT);
        match self {
            Route::Direct => builder,
            Route::Proxy { url } => builder.proxy(
                reqwest::Proxy::all(url).expect("validated proxy URL must be accepted by reqwest"),
            ),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("invalid proxy URL")]
    InvalidProxyUrl,
    #[error("proxy URL scheme must be http:// or socks5h://")]
    UnsupportedScheme,
    #[error("proxy URL must not include credentials")]
    Credentials,
    #[error("proxy URL must include a host and explicit port")]
    MissingEndpoint,
    #[error("proxy URL must not include a path, query, or fragment")]
    ExtraComponents,
}

/// Protocol detected by loopback discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyProtocol {
    HttpConnect,
    Socks5,
}

/// A discovered loopback proxy endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub route: Route,
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
}

/// Return discovered loopback proxy endpoints. This never contacts the network
/// beyond `127.0.0.1` common proxy ports.
pub fn discover() -> Vec<Endpoint> {
    discover_ports(DISCOVERY_PORTS)
}
/// Probe explicitly supplied localhost listener ports (saved custom endpoints).
/// Never scans LAN/remote addresses, and caps work to 128 distinct ports.
pub fn discover_ports(ports:impl IntoIterator<Item=u16>)->Vec<Endpoint> {
    let ports:Vec<_>=ports.into_iter().filter(|p|*p!=0).collect::<HashSet<_>>().into_iter().take(128).collect();
    let mut endpoints = Vec::new();
    let mut seen = HashSet::new();
    for port in ports {
        if probe_socks5("127.0.0.1", port) {
            push_endpoint(&mut endpoints, &mut seen, ProxyProtocol::Socks5, "127.0.0.1", port);
        }
        if probe_http_connect("127.0.0.1", port) {
            push_endpoint(
                &mut endpoints,
                &mut seen,
                ProxyProtocol::HttpConnect,
                "127.0.0.1",
                port,
            );
        }
    }
    endpoints
}

/// Current process route snapshot.
pub fn current_route() -> Route {
    route_cell().read().map(|r| r.clone()).unwrap_or_default()
}

/// Replace the process route. Existing pooled clients are dropped on the next
/// request if their snapshot no longer matches.
pub fn set_route(route: Route) {
    if let Ok(mut current) = route_cell().write() {
        *current = route;
    }
}


pub fn set_direct() {
    set_route(Route::Direct);
}

pub fn set_proxy_url(url: impl AsRef<str>) -> Result<Route, TransportError> {
    let route = Route::proxy(url)?;
    set_route(route.clone());
    Ok(route)
}

/// Builder for the current process route.
pub fn async_builder() -> reqwest::ClientBuilder {
    current_route().async_builder()
}

/// Blocking builder for the current process route.
pub fn blocking_builder() -> reqwest::blocking::ClientBuilder {
    current_route().blocking_builder()
}

/// Shared async provider client. The cache holds at most one route snapshot, so
/// switching routes drops the stale pool instead of retaining cross-route sockets.
pub(crate) fn shared_client() -> reqwest::Client {
    let route = current_route();
    let mut slot = client_slot().lock().expect("provider client slot poisoned");
    if let Some((cached_route, client)) = slot.as_ref() {
        if cached_route == &route {
            return client.clone();
        }
    }
    let client = route
        .async_builder()
        .build()
        .expect("validated provider transport route must build");
    *slot = Some((route, client.clone()));
    client
}

pub(crate) async fn http_post_sse(
    _old_client: &reqwest::Client,
    url: &str,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
    handler: EventHandler,
) -> Result<ResponseStream, ProviderError> {
    // Header names are case-insensitive; later request/auth headers override
    // catalog defaults rather than producing duplicate Authorization/User-Agent.
    let mut header_map = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| ProviderError::Decode("invalid configured HTTP header name".into()))?;
        let value = reqwest::header::HeaderValue::from_str(&v)
            .map_err(|_| ProviderError::Auth("invalid HTTP credential/header value".into()))?;
        header_map.insert(name, value);
    }

    // Ignore provider structs' construction-time client and use the current
    // route at request time, so GUI/catalog route changes take effect without
    // rebuilding provider instances.
    let client = shared_client();
    let resp = client
        .post(url)
        .headers(header_map)
        .json(&body)
        .send()
        .await
        .map_err(|e| ProviderError::Http(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::Api {
            status: status.as_u16(),
            body,
        });
    }
    let inner: super::BoxStream = Box::pin(resp.bytes_stream().map(|r| {
        r.map_err(|e| ProviderError::Http(e.to_string()))
    }));
    Ok(ResponseStream::new(inner, handler))
}

fn route_cell() -> &'static RwLock<Route> {
    static CURRENT: OnceLock<RwLock<Route>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(Route::Direct))
}

fn client_slot() -> &'static Mutex<Option<(Route, reqwest::Client)>> {
    static SLOT: OnceLock<Mutex<Option<(Route, reqwest::Client)>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn canonical_proxy_url(raw: &str) -> Result<String, TransportError> {
    let parsed = reqwest::Url::parse(raw).map_err(|_| TransportError::InvalidProxyUrl)?;
    match parsed.scheme() {
        "http" | "socks5h" => {}
        _ => return Err(TransportError::UnsupportedScheme),
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(TransportError::Credentials);
    }
    let host = parsed.host_str().ok_or(TransportError::MissingEndpoint)?;
    let port = parsed.port().ok_or(TransportError::MissingEndpoint)?;
    if !path_is_empty(parsed.path()) || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(TransportError::ExtraComponents);
    }
    let host = canonical_host(host);
    Ok(format!("{}://{}:{}", parsed.scheme(), host, port))
}

fn path_is_empty(path: &str) -> bool {
    path.is_empty() || path == "/"
}

fn canonical_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_ascii_lowercase()
    }
}

fn push_endpoint(
    endpoints: &mut Vec<Endpoint>,
    seen: &mut HashSet<u64>,
    protocol: ProxyProtocol,
    host: &str,
    port: u16,
) {
    let route = match protocol {
        ProxyProtocol::HttpConnect => Route::proxy(format!("http://{host}:{port}")),
        ProxyProtocol::Socks5 => Route::proxy(format!("socks5h://{host}:{port}")),
    };
    let Ok(route) = route else {
        return;
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    route.hash(&mut hasher);
    let key = hasher.finish();
    if seen.insert(key) {
        endpoints.push(Endpoint {
            route,
            protocol,
            host: host.to_string(),
            port,
        });
    }
}

fn probe_socks5(host: &str, port: u16) -> bool {
    let Ok(mut stream) = connect_loopback(host, port) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_IO_TIMEOUT));
    if stream.write_all(&[0x05, 0x01, 0x00]).is_err() {
        return false;
    }
    let mut response = [0_u8; 2];
    stream.read_exact(&mut response).is_ok() && response[0] == 0x05 && response[1] != 0xff
}

fn probe_http_connect(host: &str, port: u16) -> bool {
    let Ok(mut stream) = connect_loopback(host, port) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_IO_TIMEOUT));
    let request = b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n";
    if stream.write_all(request).is_err() {
        return false;
    }
    let mut buf = [0_u8; 512];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    let first_line = String::from_utf8_lossy(&buf[..n]);
    let Some(line) = first_line.lines().next() else {
        return false;
    };
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return false;
    }
    let status = parts.next().and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    matches!(status, 200 | 407 | 502 | 503)
}

fn connect_loopback(host: &str, port: u16) -> std::io::Result<TcpStream> {
    if host != "127.0.0.1" && host != "localhost" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "discovery probes are loopback-only",
        ));
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, PROBE_CONNECT_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn proxy_url_canonicalizes_and_rejects_credentials() {
        assert_eq!(
            Route::proxy("HTTP://LOCALHOST:8080/").unwrap(),
            Route::Proxy {
                url: "http://localhost:8080".into()
            }
        );
        assert!(matches!(
            Route::proxy("http://user:pass@127.0.0.1:8080"),
            Err(TransportError::Credentials)
        ));
        assert!(matches!(
            Route::proxy("socks5://127.0.0.1:1080"),
            Err(TransportError::UnsupportedScheme)
        ));
    }

    #[test]
    fn probes_local_synthetic_http_and_socks() {
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let http_port = http.local_addr().unwrap().port();
        let http_thread = thread::spawn(move || {
            let (mut socket, _) = http.accept().unwrap();
            let mut buf = [0_u8; 128];
            let _ = socket.read(&mut buf);
            socket
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        assert!(probe_http_connect("127.0.0.1", http_port));
        http_thread.join().unwrap();

        let socks = TcpListener::bind("127.0.0.1:0").unwrap();
        let socks_port = socks.local_addr().unwrap().port();
        let socks_thread = thread::spawn(move || {
            let (mut socket, _) = socks.accept().unwrap();
            let mut buf = [0_u8; 3];
            socket.read_exact(&mut buf).unwrap();
            socket.write_all(&[0x05, 0x00]).unwrap();
        });
        assert!(probe_socks5("127.0.0.1", socks_port));
        socks_thread.join().unwrap();
    }

    #[test]
    fn direct_builder_bypasses_proxy_environment() {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();

        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let thread = thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut buf = [0_u8; 512];
            let _ = socket.read(&mut buf);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });

        let old_http = std::env::var_os("HTTP_PROXY");
        let old_all = std::env::var_os("ALL_PROXY");
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:9");
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:9");
        let client = Route::Direct.blocking_builder().build().unwrap();
        restore_env("HTTP_PROXY", old_http);
        restore_env("ALL_PROXY", old_all);

        let body = client.get(url).send().unwrap().text().unwrap();
        assert_eq!(body, "ok");
        thread.join().unwrap();
    }

    fn restore_env(key: &str, old: Option<std::ffi::OsString>) {
        match old {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
