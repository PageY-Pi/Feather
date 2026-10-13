//! Persistent provider proxy route catalog.
//!
//! This module is intentionally data-only except for `discover_now` (loopback
//! probes delegated to `feather_core`) and `initialize`/selection calls that set
//! the process-local transport route. `Catalog::load` never performs network IO.

use crate::config_store;
use feather_core::provider::transport::{self, Endpoint, ProxyProtocol, Route};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const DOC_KEY: &str = "proxies";
const MAX_ENTRIES: usize = 128;

/// A durable proxy catalog entry. `active` means "seen in the most recent
/// loopback scan", not "currently selected" and not a GUI availability signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub endpoint: Endpoint,
    pub last_seen: u64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub manual: bool,
}

/// Persisted transport route catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub selected: Route,
    pub entries: Vec<CatalogEntry>,
    /// Non-fatal load/validation problems. Invalid stored selections fall back
    /// to `Direct` and report the reason here instead of silently inheriting an
    /// environment proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            selected: Route::Direct,
            entries: Vec::new(),
            error: None,
        }
    }
}

// Data-only unit tests manipulate multiple synthetic homes concurrently with
// OAuth HTTP fixtures. They must not change the process-wide real transport.
fn apply_runtime_route(route: Route) {
    #[cfg(not(test))]
    transport::set_route(route);
    #[cfg(test)]
    let _ = route;
}

/// Load the persisted proxy catalog. This does not scan ports or contact the
/// network. Missing catalog data defaults to `Direct`.
pub fn load(home: &Path) -> Catalog {
    match config_store::read(home, DOC_KEY) {
        Ok(Some(value)) => catalog_from_value(&value),
        Ok(None) => Catalog::default(),
        Err(e) => Catalog {
            selected: Route::Direct,
            entries: Vec::new(),
            error: Some(format!("failed to load proxy catalog: {e}")),
        },
    }
}

/// Run loopback proxy discovery using the core transport probe list.
pub fn discover_now() -> Vec<Endpoint> {
    transport::discover()
}
/// Include saved custom local ports such as Hiddify's mixed listener. Only
/// protocol-verified endpoints are persisted, never the REALITY remote config.
pub fn discover_for(home: &Path) -> Vec<Endpoint> {
    let mut endpoints = discover_now();
    let ports = load(home)
        .entries
        .into_iter()
        .filter(|e| matches!(e.endpoint.host.as_str(), "127.0.0.1" | "localhost" | "::1"))
        .map(|e| e.endpoint.port);
    endpoints.extend(transport::discover_ports(ports));
    endpoints.sort_by_key(|e| e.route.to_string());
    endpoints.dedup_by(|a, b| a.route == b.route);
    endpoints
}

/// Merge one scan's discovered endpoints into the persisted catalog.
///
/// New endpoints are upserted without credentials, `active` reflects only this
/// scan, and an existing selection is preserved. If the selected route is a
/// local proxy that disappeared in this scan, selection falls back to `Direct`;
/// manual external routes are never auto-selected or auto-cleared by discovery.
pub fn merge_discovered<I, E>(home: &Path, endpoints: I) -> Catalog
where
    I: IntoIterator<Item = E>,
    E: Borrow<Endpoint>,
{
    let now = now_millis();
    let mut incoming = Vec::new();
    let mut errors = Vec::new();
    for endpoint in endpoints {
        match normalize_endpoint(endpoint.borrow(), false, true, now) {
            Ok(entry) => incoming.push(entry),
            Err(e) => errors.push(e),
        }
    }

    let discovered_routes: Vec<Route> = incoming
        .iter()
        .map(|entry| entry.endpoint.route.clone())
        .collect();

    let update_result = config_store::update(home, DOC_KEY, |doc| {
        ensure_object(doc);
        let mut catalog = catalog_from_value(doc);
        for entry in &mut catalog.entries {
            entry.active = false;
        }
        for entry in incoming {
            upsert_entry(&mut catalog.entries, entry);
        }
        if selected_local_missing(&catalog.selected, &catalog.entries, &discovered_routes) {
            catalog.selected = Route::Direct;
        }
        trim_entries(&mut catalog.entries, &catalog.selected);
        doc["selected"] = route_value(&catalog.selected);
        doc["entries"] = entries_value(&catalog.entries);
        doc["lastScan"] = json!(now);
    });

    if let Err(e) = update_result {
        errors.push(format!("failed to update proxy catalog: {e}"));
    }
    let mut catalog = load(home);
    append_errors(&mut catalog, errors);
    catalog
}

/// Persist the selected route and install it in the current process. Proxy
/// routes must already exist in the catalog; use `add_manual` for external or
/// user-entered routes.
pub fn save_selection(home: &Path, route: Route) -> Result<Catalog, String> {
    let route = validate_route(&route)?;
    let current = load(home);
    if matches!(route, Route::Proxy { .. })
        && !current
            .entries
            .iter()
            .any(|entry| entry.endpoint.route == route)
    {
        return Err("proxy endpoint is not in the catalog; add it first".into());
    }
    config_store::update(home, DOC_KEY, |doc| {
        ensure_object(doc);
        doc["selected"] = route_value(&route);
    })
    .map_err(|e| format!("failed to save proxy selection: {e}"))?;
    apply_runtime_route(route);
    Ok(load(home))
}

/// Add or update a manually entered proxy endpoint. The URL is canonicalized and
/// credentials, paths, queries and fragments are rejected by core transport.
pub fn add_manual(home: &Path, url: &str) -> Result<Catalog, String> {
    let route = Route::proxy(url).map_err(|e| e.to_string())?;
    add_manual_route(home, route)
}

/// Add or update a manually entered proxy route.
pub fn add_manual_route(home: &Path, route: Route) -> Result<Catalog, String> {
    let route = validate_route(&route)?;
    let endpoint = endpoint_from_route(&route)?;
    let entry = CatalogEntry {
        endpoint,
        last_seen: now_millis(),
        active: false,
        manual: true,
    };
    config_store::update(home, DOC_KEY, |doc| {
        ensure_object(doc);
        let mut catalog = catalog_from_value(doc);
        upsert_entry(&mut catalog.entries, entry);
        trim_entries(&mut catalog.entries, &catalog.selected);
        doc["selected"] = route_value(&catalog.selected);
        doc["entries"] = entries_value(&catalog.entries);
    })
    .map_err(|e| format!("failed to save manual proxy endpoint: {e}"))?;
    Ok(load(home))
}

/// Remove a proxy endpoint. If it was selected, selection falls back to Direct.
pub fn remove(home: &Path, route: Route) -> Result<Catalog, String> {
    let route = validate_route(&route)?;
    config_store::update(home, DOC_KEY, |doc| {
        ensure_object(doc);
        let mut catalog = catalog_from_value(doc);
        catalog
            .entries
            .retain(|entry| entry.endpoint.route != route);
        if catalog.selected == route {
            catalog.selected = Route::Direct;
        }
        doc["selected"] = route_value(&catalog.selected);
        doc["entries"] = entries_value(&catalog.entries);
    })
    .map_err(|e| format!("failed to remove proxy endpoint: {e}"))?;
    let catalog = load(home);
    if catalog.selected == Route::Direct {
        apply_runtime_route(Route::Direct);
    }
    Ok(catalog)
}

/// Install the persisted selected route once for this process. This is intended
/// for CLI startup; GUI parents can load/merge/save explicitly.
pub fn initialize(home: &Path) -> Catalog {
    static INITIALIZED: OnceLock<()> = OnceLock::new();
    let catalog = load(home);
    INITIALIZED.get_or_init(|| {
        apply_runtime_route(catalog.selected.clone());
    });
    catalog
}

/// Current process route snapshot.
pub fn current_route() -> Route {
    transport::current_route()
}

fn catalog_from_value(value: &Value) -> Catalog {
    let mut catalog = Catalog::default();
    let Some(object) = value.as_object() else {
        catalog.error = Some("proxy catalog document is invalid; using Direct".into());
        return catalog;
    };

    if let Some(selected) = object.get("selected") {
        match route_from_value(selected) {
            Ok(route) => catalog.selected = route,
            Err(e) => {
                catalog.error = Some(format!("invalid stored proxy selection: {e}; using Direct"))
            }
        }
    }

    if let Some(entries) = object.get("entries").and_then(Value::as_array) {
        for value in entries.iter().take(MAX_ENTRIES.saturating_mul(2)) {
            match entry_from_value(value) {
                Ok(entry) => upsert_entry(&mut catalog.entries, entry),
                Err(e) => append_errors(
                    &mut catalog,
                    vec![format!("ignored invalid proxy entry: {e}")],
                ),
            }
        }
    }
    trim_entries(&mut catalog.entries, &catalog.selected);
    catalog
}

fn entry_from_value(value: &Value) -> Result<CatalogEntry, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "entry is not an object".to_string())?;
    let endpoint_value = object.get("endpoint").unwrap_or(value);
    let endpoint = endpoint_from_value(endpoint_value)?;
    let last_seen = object
        .get("lastSeen")
        .or_else(|| object.get("last_seen"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let active = object
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let manual = object
        .get("manual")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(CatalogEntry {
        endpoint,
        last_seen,
        active,
        manual,
    })
}

fn endpoint_from_value(value: &Value) -> Result<Endpoint, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "endpoint is not an object".to_string())?;
    let route_value = object
        .get("route")
        .ok_or_else(|| "endpoint route missing".to_string())?;
    let route = route_from_value(route_value)?;
    let mut endpoint = endpoint_from_route(&route)?;
    if let Some(protocol_value) = object.get("protocol") {
        let protocol: ProxyProtocol = serde_json::from_value(protocol_value.clone())
            .map_err(|e| format!("invalid protocol: {e}"))?;
        endpoint.protocol = protocol;
    }
    if let Some(host) = object.get("host").and_then(Value::as_str) {
        endpoint.host = host.to_string();
    }
    if let Some(port) = object.get("port").and_then(Value::as_u64) {
        endpoint.port =
            u16::try_from(port).map_err(|_| "endpoint port out of range".to_string())?;
    }
    Ok(endpoint)
}

fn normalize_endpoint(
    endpoint: &Endpoint,
    manual: bool,
    active: bool,
    last_seen: u64,
) -> Result<CatalogEntry, String> {
    let route = validate_route(&endpoint.route)?;
    let mut clean = endpoint_from_route(&route)?;
    clean.protocol = endpoint.protocol;
    clean.host = endpoint.host.clone();
    clean.port = endpoint.port;
    Ok(CatalogEntry {
        endpoint: clean,
        last_seen,
        active,
        manual,
    })
}

fn endpoint_from_route(route: &Route) -> Result<Endpoint, String> {
    let route = validate_route(route)?;
    let url = route
        .proxy_url()
        .ok_or_else(|| "Direct is not a proxy endpoint".to_string())?;
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid proxy URL".to_string())?;
    let protocol = match parsed.scheme() {
        "http" => ProxyProtocol::HttpConnect,
        "socks5h" => ProxyProtocol::Socks5,
        _ => return Err("unsupported proxy scheme".into()),
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| "proxy host missing".to_string())?
        .to_string();
    let port = parsed
        .port()
        .ok_or_else(|| "proxy port missing".to_string())?;
    Ok(Endpoint {
        route,
        protocol,
        host,
        port,
    })
}

fn validate_route(route: &Route) -> Result<Route, String> {
    match route {
        Route::Direct => Ok(Route::Direct),
        Route::Proxy { url } => Route::proxy(url).map_err(|e| e.to_string()),
    }
}

fn route_from_value(value: &Value) -> Result<Route, String> {
    if let Some(s) = value.as_str() {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty route".into());
        }
        return Route::from_str(s).map_err(|e| e.to_string());
    }
    let route: Route = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    validate_route(&route)
}

fn route_value(route: &Route) -> Value {
    serde_json::to_value(route).unwrap_or_else(|_| json!("Direct"))
}

fn entries_value(entries: &[CatalogEntry]) -> Value {
    serde_json::to_value(entries).unwrap_or_else(|_| json!([]))
}

fn upsert_entry(entries: &mut Vec<CatalogEntry>, entry: CatalogEntry) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|existing| existing.endpoint.route == entry.endpoint.route)
    {
        existing.endpoint = entry.endpoint;
        existing.last_seen = entry.last_seen.max(existing.last_seen);
        existing.active = entry.active;
        existing.manual |= entry.manual;
    } else {
        entries.push(entry);
    }
}

fn trim_entries(entries: &mut Vec<CatalogEntry>, selected: &Route) {
    entries.sort_by(|a, b| compare_entries(a, b, selected));
    if entries.len() > MAX_ENTRIES {
        eprintln!(
            "warning: proxy catalog has more than {MAX_ENTRIES} entries; trimming oldest entries"
        );
        entries.truncate(MAX_ENTRIES);
    }
}

fn compare_entries(a: &CatalogEntry, b: &CatalogEntry, selected: &Route) -> Ordering {
    let a_selected = a.endpoint.route == *selected;
    let b_selected = b.endpoint.route == *selected;
    b_selected
        .cmp(&a_selected)
        .then_with(|| b.active.cmp(&a.active))
        .then_with(|| b.last_seen.cmp(&a.last_seen))
        .then_with(|| {
            a.endpoint
                .route
                .to_string()
                .cmp(&b.endpoint.route.to_string())
        })
}

fn selected_local_missing(
    selected: &Route,
    entries: &[CatalogEntry],
    discovered: &[Route],
) -> bool {
    if !route_is_loopback(selected) {
        return false;
    }
    let known = entries
        .iter()
        .any(|entry| entry.endpoint.route == *selected && !entry.manual);
    known && !discovered.iter().any(|route| route == selected)
}

fn route_is_loopback(route: &Route) -> bool {
    let Some(url) = route.proxy_url() else {
        return false;
    };
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host == "127.0.0.1"
            || host == "[::1]"
            || host == "::1"
    })
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = json!({});
    }
}

fn append_errors(catalog: &mut Catalog, errors: Vec<String>) {
    if errors.is_empty() {
        return;
    }
    let mut combined = catalog.error.take().unwrap_or_default();
    for error in errors {
        if !combined.is_empty() {
            combined.push_str("; ");
        }
        combined.push_str(&error);
    }
    catalog.error = Some(combined);
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn tmp_home(label: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!(
            "feather-proxy-{label}-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn endpoint(url: &str) -> Endpoint {
        endpoint_from_route(&Route::proxy(url).unwrap()).unwrap()
    }

    #[test]
    fn merge_two_homes_preserves_selection_and_unknown_fields() {
        let a = tmp_home("a");
        let b = tmp_home("b");
        config_store::write(&a, DOC_KEY, &json!({"selected":"direct","unknown":42})).unwrap();
        merge_discovered(&a, vec![endpoint("http://127.0.0.1:7890")]);
        let stored = config_store::read(&a, DOC_KEY).unwrap().unwrap();
        assert_eq!(stored["unknown"], 42);
        assert_eq!(load(&a).selected, Route::Direct);
        assert_eq!(load(&a).entries.len(), 1);

        add_manual(&b, "socks5h://example.com:1080").unwrap();
        let manual = Route::proxy("socks5h://example.com:1080").unwrap();
        save_selection(&b, manual.clone()).unwrap();
        merge_discovered(&b, Vec::<Endpoint>::new());
        assert_eq!(load(&b).selected, manual);
        assert!(load(&a).entries.iter().all(|e| e.endpoint.port != 1080));
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }

    #[test]
    fn credential_rejection_and_local_fallback() {
        let home = tmp_home("reject");
        assert!(add_manual(&home, "http://user:pass@127.0.0.1:7890").is_err());
        let route = Route::proxy("http://127.0.0.1:7890").unwrap();
        merge_discovered(&home, vec![endpoint("http://127.0.0.1:7890")]);
        save_selection(&home, route).unwrap();
        merge_discovered(&home, Vec::<Endpoint>::new());
        assert_eq!(load(&home).selected, Route::Direct);
        let manual = Route::proxy("socks5h://127.0.0.1:32001").unwrap();
        add_manual_route(&home, manual.clone()).unwrap();
        save_selection(&home, manual.clone()).unwrap();
        merge_discovered(&home, Vec::<Endpoint>::new());
        assert_eq!(
            load(&home).selected,
            manual,
            "a scan of common ports must not clear a manually entered noncommon endpoint"
        );
        fs::remove_dir_all(home).unwrap();
    }
}
