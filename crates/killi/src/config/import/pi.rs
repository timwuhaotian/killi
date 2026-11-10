use super::*;
use std::path::Path;

use anyhow::{Result, bail};

/// Copies providers, models, the default model, the thinking level and MCP servers from
/// pi's config (`~/.pi/agent`). A provider pi reaches through its account login has no key
/// in `models.json` and is skipped, as Kimi Code's account providers are.
pub fn import_pi(force: bool) -> Result<()> {
    import_pi_from(&home().join(".pi/agent"), &killi_home(), force)
}

pub(crate) fn import_pi_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(config) = read_json(&dir.join("models.json"))? else {
        bail!("no models.json in {} to import", dir.display())
    };
    let catalog = read_json(&dir.join("models-store.json"))?.unwrap_or_default();
    let settings = read_json(&dir.join("settings.json"))?.unwrap_or_default();
    let configured = json_str(&settings, "defaultProvider")
        .zip(json_str(&settings, "defaultModel"))
        .map(|(p, m)| format!("{p}/{m}"));
    let effort = json_str(&settings, "defaultThinkingLevel").filter(|level| level != "auto");
    import_pi_shape(
        dir,
        dst_dir,
        force,
        &config,
        &catalog,
        configured.as_deref(),
        effort.as_deref(),
        "pi",
        PiKey::Pi,
    )
}

/// How a pi-shaped `apiKey` reads. pi's is the key itself; omp's names an environment
/// variable when one is set, and runs as a shell command when it starts with `!` — that
/// spelling killi won't copy, so the provider is skipped like an account login.
enum PiKey {
    Pi,
    Omp,
}

/// The `api_key`/`api_key_env` pair a pi-shaped provider's `apiKey` becomes, or `None`
/// when there is no key killi can use.
fn pi_key_fields(provider: &serde_json::Value, key: &PiKey) -> Option<(&'static str, String)> {
    let raw = json_str(provider, "apiKey")?;
    match key {
        PiKey::Pi => Some(("api_key", raw)),
        PiKey::Omp if raw.starts_with('!') => None,
        PiKey::Omp if std::env::var(&raw).is_ok_and(|value| !value.is_empty()) => {
            Some(("api_key_env", raw))
        }
        PiKey::Omp => Some(("api_key", raw)),
    }
}

/// Writes a pi-shaped agent's config — pi's `models.json`, omp's YAML read the same way —
/// as killi's: each provider with a key and its models, the default and effort the caller
/// found, and its MCP servers copied across.
#[allow(clippy::too_many_arguments)]
fn import_pi_shape(
    dir: &Path,
    dst_dir: &Path,
    force: bool,
    config: &serde_json::Value,
    catalog: &serde_json::Value,
    configured: Option<&str>,
    effort: Option<&str>,
    source: &str,
    key: PiKey,
) -> Result<()> {
    let Some(all) = config
        .get("providers")
        .and_then(serde_json::Value::as_object)
    else {
        bail!("no providers in {}", dir.display())
    };

    let mut out = toml::Table::new();
    let mut providers = toml::Table::new();
    let mut models = toml::Table::new();
    let mut skipped = Vec::new();
    for (name, provider) in all {
        let Some((key_field, api_key)) = pi_key_fields(provider, &key) else {
            skipped.push(name.clone());
            continue;
        };
        let listed = pi_models(
            provider,
            catalog.get(name).unwrap_or(&serde_json::Value::Null),
        );
        if listed.is_empty() {
            skipped.push(name.clone());
            continue;
        }
        let kind = pi_protocol(
            json_str(provider, "api")
                .as_deref()
                .or(listed[0].api.as_deref()),
        );
        let mut table = toml::Table::new();
        table.insert("type".into(), kind.into());
        if let Some(url) = json_str(provider, "baseUrl").or_else(|| listed[0].base_url.clone()) {
            table.insert("base_url".into(), url.into());
        }
        table.insert(key_field.into(), api_key.into());
        if let Some(headers) = provider
            .get("headers")
            .and_then(serde_json::Value::as_object)
        {
            let mut custom = toml::Table::new();
            for (header, value) in headers {
                if let Some(value) = value.as_str() {
                    custom.insert(header.clone(), value.into());
                }
            }
            if !custom.is_empty() {
                table.insert("custom_headers".into(), toml::Value::Table(custom));
            }
        }
        providers.insert(name.clone(), toml::Value::Table(table));

        for model in &listed {
            let mut table = toml::Table::new();
            table.insert("provider".into(), name.clone().into());
            table.insert("model".into(), model.id.clone().into());
            if let Some(api) = model.api.as_deref() {
                let protocol = pi_protocol(Some(api));
                if protocol != kind {
                    table.insert("protocol".into(), protocol.into());
                }
            }
            if let Some(context) = model.context {
                table.insert("max_context_size".into(), toml_int(context));
            }
            if let Some(output) = model.output {
                table.insert("max_output_size".into(), toml_int(output));
            }
            let mut capabilities = Vec::new();
            if model.reasoning {
                capabilities.push("thinking".to_string());
            }
            if model.image {
                capabilities.push("image_in".to_string());
            }
            if !capabilities.is_empty() {
                table.insert("capabilities".into(), capabilities.into());
            }
            models.insert(format!("{name}/{}", model.id), toml::Value::Table(table));
        }
    }
    if providers.is_empty() {
        bail!("{source}'s config has no provider with an API key to import");
    }

    let default = match configured {
        Some(alias) if models.contains_key(alias) => Some(alias.to_string()),
        _ => models.keys().next().cloned(),
    };
    if let Some(alias) = &default {
        out.insert("default_model".into(), alias.clone().into());
        if configured != Some(alias.as_str()) {
            eprintln!(
                "{source}'s default model {} wasn't imported, so `{alias}` is the default",
                configured.unwrap_or("(none)")
            );
        }
        // The thinking level the agent had is per model here, so it lands on the default.
        if let Some(effort) = effort
            && let Some(toml::Value::Table(model)) = models.get_mut(alias)
        {
            model.insert("default_effort".into(), effort.to_string().into());
        }
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, Some(&dir.join("mcp.json")), dst_dir, force)?;
    println!(
        "Imported {provider_count} providers and {model_count} models into {}",
        dst_dir.join("config.toml").display()
    );
    if !skipped.is_empty() {
        println!(
            "Skipped providers without an API key ({source} account login): {}",
            skipped.join(", ")
        );
    }
    Ok(())
}

/// One model as pi lists it, in a provider's own `models` or the `models-store.json` catalog.
struct PiModel {
    id: String,
    /// The API the model speaks, when it says so; else the provider's.
    api: Option<String>,
    base_url: Option<String>,
    context: Option<u64>,
    output: Option<u64>,
    reasoning: bool,
    image: bool,
}

/// The provider's own models, or its catalog's when it lists none.
fn pi_models(provider: &serde_json::Value, catalog: &serde_json::Value) -> Vec<PiModel> {
    let inline = provider
        .get("models")
        .and_then(serde_json::Value::as_array)
        .filter(|models| !models.is_empty());
    let listed: Vec<&serde_json::Value> = match inline {
        Some(models) => models.iter().collect(),
        None => catalog
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(|m| m.iter().collect())
            .unwrap_or_default(),
    };
    listed
        .iter()
        .filter_map(|model| {
            Some(PiModel {
                id: json_str(model, "id")?,
                api: json_str(model, "api"),
                base_url: json_str(model, "baseUrl"),
                context: json_u64(model, "contextWindow"),
                output: json_u64(model, "maxTokens"),
                reasoning: model
                    .get("reasoning")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                image: model
                    .get("input")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|input| input.iter().any(|kind| kind.as_str() == Some("image"))),
            })
        })
        .collect()
}

/// The API pi (or omp) names for a provider or model, as killi's provider type.
fn pi_protocol(api: Option<&str>) -> &'static str {
    match api {
        Some("openai-responses" | "openai-codex-responses" | "azure-openai-responses") => {
            "openai_responses"
        }
        Some("anthropic-messages") => "anthropic",
        _ => "openai",
    }
}

/// Copies providers, models, the default model, the thinking level and MCP servers from
/// omp's config (`~/.omp/agent`). omp writes pi's model shape as YAML (`models.yml`, with
/// the default in `config.yml`), so the import reads it into the same value tree pi's JSON
/// makes and shares pi's mapping. Only the providers omp's models file defines come over:
/// the ones it builds in — reached with an environment variable or its account login —
/// carry no endpoint or key in the file. A leftover pi-style `models.json`/`settings.json`
/// in the directory is read when the YAML isn't there.
pub fn import_omp(force: bool) -> Result<()> {
    import_omp_from(&home().join(".omp/agent"), &killi_home(), force)
}

pub(crate) fn import_omp_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(config) = read_yaml(&dir.join("models.yml"))?.or(read_json(&dir.join("models.json"))?)
    else {
        bail!(
            "no models.yml or models.json in {} to import",
            dir.display()
        )
    };
    let catalog = read_json(&dir.join("models-store.json"))?.unwrap_or_default();
    let settings = read_yaml(&dir.join("config.yml"))?
        .or(read_json(&dir.join("settings.json"))?)
        .unwrap_or_default();
    let (configured, effort) = omp_default(&settings);
    import_pi_shape(
        dir,
        dst_dir,
        force,
        &config,
        &catalog,
        configured.as_deref(),
        effort.as_deref(),
        "omp",
        PiKey::Omp,
    )
}

/// omp's configured default model and thinking level: its `modelRoles.default` (the first
/// of a comma list, with any `:effort` off its end), else the pi-era settings it migrates
/// from. A role alias (`@smol`) or a glob names no one model, so pressing on falls back.
fn omp_default(settings: &serde_json::Value) -> (Option<String>, Option<String>) {
    let role = settings
        .get("modelRoles")
        .and_then(|roles| json_str(roles, "default"));
    if let Some(role) = role.filter(|value| !value.starts_with('@') && !value.starts_with('*')) {
        let first = role.split(',').next().unwrap_or_default().trim();
        let (alias, effort) = match first.rsplit_once(':') {
            Some((alias, effort)) if !effort.is_empty() && effort != "auto" => {
                (alias, Some(effort.to_string()))
            }
            _ => (first, None),
        };
        return ((!alias.is_empty()).then(|| alias.to_string()), effort);
    }
    let configured = json_str(settings, "defaultProvider")
        .zip(json_str(settings, "defaultModel"))
        .map(|(p, m)| format!("{p}/{m}"));
    let effort = json_str(settings, "defaultThinkingLevel").filter(|level| level != "auto");
    (configured, effort)
}
