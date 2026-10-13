//! Safely trigger redb configuration migrations and print counts only.
fn main() -> anyhow::Result<()> {
    let home = feather::config::feather_home();
    let saved = feather::models::saved_for(&home)?;
    let count: usize = saved.providers.values().map(|p| p.models.len()).sum();
    println!(
        "Migrated config.redb: {} providers, {count} saved models. No discovery requests.",
        saved.providers.len()
    );
    Ok(())
}
