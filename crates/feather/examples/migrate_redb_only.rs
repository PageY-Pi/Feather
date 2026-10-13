//! Explicit, secret-safe migration/admin helper: no account/token/config dumps.
fn main() -> anyhow::Result<()> {
    let home = feather::config::feather_home();
    for key in [
        "settings",
        "models",
        "auth",
        "keybindings",
        "subagent-policy",
        "compaction-model",
        "gui",
        "proxies",
    ] {
        let present = feather::config_store::read(&home, key)?.is_some();
        println!("{key}: redb document present={present}");
    }
    let auth = feather::oauth::AuthStore::load_from(&home.join("config.redb"))
        .map_err(anyhow::Error::msg)?;
    println!(
        "Auth store providers={} (credential values withheld)",
        auth.0.len()
    );
    let proxy = feather_core::provider::transport::Route::proxy("socks5h://127.0.0.1:12334")?;
    let found = feather_core::provider::transport::discover_ports([12334]);
    anyhow::ensure!(
        found.iter().any(|e| e.route == proxy),
        "Hiddify local SOCKS listener not active; refusing selection"
    );
    feather::proxy::add_manual_route(&home, proxy.clone()).map_err(anyhow::Error::msg)?;
    feather::proxy::save_selection(&home, proxy.clone()).map_err(anyhow::Error::msg)?;
    println!("Selected Hiddify local proxy {proxy} in redb (REALITY remote settings untouched)");
    Ok(())
}
