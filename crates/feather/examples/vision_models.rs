//! Safe catalog capability listing for vision reviewer choice. Outputs only
//! model ids, names and image flags; NEVER credentials/base URLs/headers/config.
fn main() -> anyhow::Result<()> {
    let catalog =
        feather::models::load().ok_or_else(|| anyhow::anyhow!("model catalog unavailable"))?;
    for (pid, provider) in catalog.providers {
        for model in provider.models {
            if model.id.to_ascii_lowercase().contains("deepseek")
                || pid.to_ascii_lowercase().contains("deepseek")
            {
                println!(
                    "{pid}/{} | {} | image={}",
                    model.id,
                    model.name.as_deref().unwrap_or(""),
                    model.input.iter().any(|i| i == "image")
                );
            }
        }
    }
    Ok(())
}
