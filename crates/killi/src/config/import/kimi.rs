use super::*;
use std::fs;

use anyhow::{Context, Result, bail};

/// Copies providers, models, `default_model` and thinking from Kimi Code's config: its
/// `[thinking] enabled` comes over as is, and its effort becomes the default model's
/// `default_effort`. Providers that sign in through Kimi's account login are skipped.
pub fn import_kimi(force: bool) -> Result<()> {
    let src = home().join(".kimi-code/config.toml");
    let dir = killi_home();
    let dst = dir.join("config.toml");
    if dst.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            dst.display()
        );
    }
    let text =
        fs::read_to_string(&src).with_context(|| format!("could not read {}", src.display()))?;
    let kimi: toml::Table =
        toml::from_str(&text).with_context(|| format!("invalid TOML in {}", src.display()))?;

    let mut providers = toml::Table::new();
    let mut skipped = Vec::new();
    if let Some(toml::Value::Table(all)) = kimi.get("providers") {
        for (name, provider) in all {
            let kind = provider
                .get("type")
                .and_then(toml::Value::as_str)
                .unwrap_or_default();
            let has_key =
                provider.get("api_key").is_some() || provider.get("api_key_env").is_some();
            if kind == "kimi" || !has_key {
                skipped.push(name.clone());
                continue;
            }
            let mut provider = provider.as_table().cloned().unwrap_or_default();
            provider.remove("oauth");
            providers.insert(name.clone(), toml::Value::Table(provider));
        }
    }

    let mut models = toml::Table::new();
    if let Some(toml::Value::Table(all)) = kimi.get("models") {
        for (alias, model) in all {
            let provider = model
                .get("provider")
                .and_then(toml::Value::as_str)
                .unwrap_or_default();
            if providers.contains_key(provider) {
                models.insert(alias.clone(), model.clone());
            }
        }
    }

    let mut out = toml::Table::new();
    let mut default = None;
    if let Some(configured) = kimi.get("default_model").and_then(toml::Value::as_str) {
        if models.contains_key(configured) {
            out.insert("default_model".into(), configured.into());
            default = Some(configured.to_string());
        } else {
            eprintln!("default_model `{configured}` was not imported, so none is set");
        }
    }
    if let Some(thinking) = kimi.get("thinking") {
        let mut ours = toml::Table::new();
        if let Some(enabled) = thinking.get("enabled") {
            ours.insert("enabled".into(), enabled.clone());
        }
        if let (Some(alias), Some(effort)) = (
            default.as_deref(),
            thinking.get("effort").and_then(toml::Value::as_str),
        ) && let Some(toml::Value::Table(model)) = models.get_mut(alias)
        {
            model.insert("default_effort".into(), effort.to_string().into());
        }
        if !ours.is_empty() {
            out.insert("thinking".into(), ours.into());
        }
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(
        out,
        Some(&home().join(".kimi-code/mcp.json")),
        &killi_home(),
        force,
    )?;
    println!(
        "Imported {provider_count} providers and {model_count} models into {}",
        dst.display()
    );
    if !skipped.is_empty() {
        println!(
            "Skipped providers without an API key (Kimi account login): {}",
            skipped.join(", ")
        );
    }
    Ok(())
}
