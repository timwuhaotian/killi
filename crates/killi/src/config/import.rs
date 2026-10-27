use super::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Writes an imported config with the `[prompt]` table, and copies the source agent's MCP
/// servers when killi has none. Every importer ends here.
fn write_import(out: toml::Table, mcp_src: Option<&Path>, dst_dir: &Path, force: bool) -> Result<()> {
    let dst = dst_dir.join("config.toml");
    if dst.exists() && !force {
        bail!("{} already exists; pass --force to overwrite it", dst.display());
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dst_dir)?;
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&dst)?;
    file.write_all(toml::to_string(&out)?.as_bytes())?;
    file.write_all(prompt_section().as_bytes())?;
    if let Some(src) = mcp_src.filter(|src| src.exists()) {
        let mcp_dst = dst_dir.join("mcp.json");
        if !mcp_dst.exists() || force {
            fs::copy(src, &mcp_dst).with_context(|| format!("could not copy {} to {}", src.display(), mcp_dst.display()))?;
            println!("Copied MCP servers into {}", mcp_dst.display());
        }
    }
    Ok(())
}

/// Copies providers, models, `default_model` and `[thinking]` from Kimi Code's config.
/// Providers that sign in through Kimi's account login are skipped.
pub fn import_kimi(force: bool) -> Result<()> {
    let src = home().join(".kimi-code/config.toml");
    let dir = killi_home();
    let dst = dir.join("config.toml");
    if dst.exists() && !force {
        bail!("{} already exists; pass --force to overwrite it", dst.display());
    }
    let text = fs::read_to_string(&src).with_context(|| format!("could not read {}", src.display()))?;
    let kimi: toml::Table = toml::from_str(&text).with_context(|| format!("invalid TOML in {}", src.display()))?;

    let mut providers = toml::Table::new();
    let mut skipped = Vec::new();
    if let Some(toml::Value::Table(all)) = kimi.get("providers") {
        for (name, provider) in all {
            let kind = provider.get("type").and_then(toml::Value::as_str).unwrap_or_default();
            let has_key = provider.get("api_key").is_some() || provider.get("api_key_env").is_some();
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
            let provider = model.get("provider").and_then(toml::Value::as_str).unwrap_or_default();
            if providers.contains_key(provider) {
                models.insert(alias.clone(), model.clone());
            }
        }
    }

    let mut out = toml::Table::new();
    if let Some(default) = kimi.get("default_model").and_then(toml::Value::as_str) {
        if models.contains_key(default) {
            out.insert("default_model".into(), default.into());
        } else {
            eprintln!("default_model `{default}` was not imported, so none is set");
        }
    }
    if let Some(thinking) = kimi.get("thinking") {
        out.insert("thinking".into(), thinking.clone());
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, Some(&home().join(".kimi-code/mcp.json")), &killi_home(), force)?;
    println!("Imported {provider_count} providers and {model_count} models into {}", dst.display());
    if !skipped.is_empty() {
        println!("Skipped providers without an API key (Kimi account login): {}", skipped.join(", "));
    }
    Ok(())
}

/// Copies providers, models, the default model, the thinking level and MCP servers from
/// pi's config (`~/.pi/agent`). A provider pi reaches through its account login has no key
/// in `models.json` and is skipped, as Kimi Code's account providers are.
pub fn import_pi(force: bool) -> Result<()> {
    import_pi_from(&home().join(".pi/agent"), &killi_home(), force)
}

pub(super) fn import_pi_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(config) = read_json(&dir.join("models.json"))? else {
        bail!("no models.json in {} to import", dir.display())
    };
    let catalog = read_json(&dir.join("models-store.json"))?.unwrap_or_default();
    let settings = read_json(&dir.join("settings.json"))?.unwrap_or_default();
    let configured = json_str(&settings, "defaultProvider").zip(json_str(&settings, "defaultModel")).map(|(p, m)| format!("{p}/{m}"));
    let effort = json_str(&settings, "defaultThinkingLevel").filter(|level| level != "auto");
    import_pi_shape(dir, dst_dir, force, &config, &catalog, configured.as_deref(), effort.as_deref(), "pi", PiKey::Pi)
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
        PiKey::Omp if std::env::var(&raw).is_ok_and(|value| !value.is_empty()) => Some(("api_key_env", raw)),
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
    let Some(all) = config.get("providers").and_then(serde_json::Value::as_object) else {
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
        let listed = pi_models(provider, catalog.get(name).unwrap_or(&serde_json::Value::Null));
        if listed.is_empty() {
            skipped.push(name.clone());
            continue;
        }
        let kind = pi_protocol(json_str(provider, "api").as_deref().or(listed[0].api.as_deref()));
        let mut table = toml::Table::new();
        table.insert("type".into(), kind.into());
        if let Some(url) = json_str(provider, "baseUrl").or_else(|| listed[0].base_url.clone()) {
            table.insert("base_url".into(), url.into());
        }
        table.insert(key_field.into(), api_key.into());
        if let Some(headers) = provider.get("headers").and_then(serde_json::Value::as_object) {
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
            eprintln!("{source}'s default model {} wasn't imported, so `{alias}` is the default", configured.unwrap_or("(none)"));
        }
    }
    if let Some(effort) = effort {
        out.insert("thinking".into(), toml::Table::from_iter([("effort".to_string(), effort.into())]).into());
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, Some(&dir.join("mcp.json")), dst_dir, force)?;
    println!("Imported {provider_count} providers and {model_count} models into {}", dst_dir.join("config.toml").display());
    if !skipped.is_empty() {
        println!("Skipped providers without an API key ({source} account login): {}", skipped.join(", "));
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
    let inline = provider.get("models").and_then(serde_json::Value::as_array).filter(|models| !models.is_empty());
    let listed: Vec<&serde_json::Value> = match inline {
        Some(models) => models.iter().collect(),
        None => catalog.get("models").and_then(serde_json::Value::as_array).map(|m| m.iter().collect()).unwrap_or_default(),
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
                reasoning: model.get("reasoning").and_then(serde_json::Value::as_bool).unwrap_or(false),
                image: model.get("input").and_then(serde_json::Value::as_array).is_some_and(|input| input.iter().any(|kind| kind.as_str() == Some("image"))),
            })
        })
        .collect()
}

/// The API pi (or omp) names for a provider or model, as killi's provider type.
fn pi_protocol(api: Option<&str>) -> &'static str {
    match api {
        Some("openai-responses" | "openai-codex-responses" | "azure-openai-responses") => "openai_responses",
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

pub(super) fn import_omp_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(config) = read_yaml(&dir.join("models.yml"))?.or(read_json(&dir.join("models.json"))?) else {
        bail!("no models.yml or models.json in {} to import", dir.display())
    };
    let catalog = read_json(&dir.join("models-store.json"))?.unwrap_or_default();
    let settings = read_yaml(&dir.join("config.yml"))?.or(read_json(&dir.join("settings.json"))?).unwrap_or_default();
    let (configured, effort) = omp_default(&settings);
    import_pi_shape(dir, dst_dir, force, &config, &catalog, configured.as_deref(), effort.as_deref(), "omp", PiKey::Omp)
}

/// omp's configured default model and thinking level: its `modelRoles.default` (the first
/// of a comma list, with any `:effort` off its end), else the pi-era settings it migrates
/// from. A role alias (`@smol`) or a glob names no one model, so pressing on falls back.
fn omp_default(settings: &serde_json::Value) -> (Option<String>, Option<String>) {
    let role = settings.get("modelRoles").and_then(|roles| json_str(roles, "default"));
    if let Some(role) = role.filter(|value| !value.starts_with('@') && !value.starts_with('*')) {
        let first = role.split(',').next().unwrap_or_default().trim();
        let (alias, effort) = match first.rsplit_once(':') {
            Some((alias, effort)) if !effort.is_empty() && effort != "auto" => (alias, Some(effort.to_string())),
            _ => (first, None),
        };
        return ((!alias.is_empty()).then(|| alias.to_string()), effort);
    }
    let configured = json_str(settings, "defaultProvider").zip(json_str(settings, "defaultModel")).map(|(p, m)| format!("{p}/{m}"));
    let effort = json_str(settings, "defaultThinkingLevel").filter(|level| level != "auto");
    (configured, effort)
}

/// Copies providers, models, the default model and MCP servers from opencode's config
/// (`~/.config/opencode/opencode.json`, with keys from `~/.local/share/opencode/auth.json`).
/// Providers opencode signs into with OAuth have no key to copy and are skipped.
pub fn import_opencode(force: bool) -> Result<()> {
    import_opencode_from(&home().join(".config/opencode/opencode.json"), &home().join(".local/share/opencode/auth.json"), &killi_home(), force)
}

pub(super) fn import_opencode_from(config_path: &Path, auth_path: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(config) = read_json(config_path)? else {
        bail!("no opencode config at {} to import", config_path.display())
    };
    let auth = read_json(auth_path)?.unwrap_or_default();
    let Some(all) = config.get("provider").and_then(serde_json::Value::as_object) else {
        bail!("no providers in {}", config_path.display())
    };

    let mut out = toml::Table::new();
    let mut providers = toml::Table::new();
    let mut models = toml::Table::new();
    let mut skipped = Vec::new();
    for (name, provider) in all {
        let Some(api_key) = opencode_key(provider, &auth, name) else {
            skipped.push(name.clone());
            continue;
        };
        let Some(listed) = provider.get("models").and_then(serde_json::Value::as_object).filter(|models| !models.is_empty()) else {
            skipped.push(name.clone());
            continue;
        };
        let mut table = toml::Table::new();
        table.insert("type".into(), opencode_protocol(json_str(provider, "npm").as_deref()).into());
        if let Some(url) = provider.get("options").and_then(|options| json_str(options, "baseURL")) {
            table.insert("base_url".into(), url.into());
        }
        table.insert("api_key".into(), api_key.into());
        providers.insert(name.clone(), toml::Value::Table(table));

        for (id, model) in listed {
            let mut table = toml::Table::new();
            table.insert("provider".into(), name.clone().into());
            table.insert("model".into(), id.clone().into());
            let limits = model.get("limit");
            if let Some(context) = limits.and_then(|limits| json_u64(limits, "context")) {
                table.insert("max_context_size".into(), toml_int(context));
            }
            if let Some(output) = limits.and_then(|limits| json_u64(limits, "output")) {
                table.insert("max_output_size".into(), toml_int(output));
            }
            let mut capabilities = Vec::new();
            if model.get("reasoning").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                capabilities.push("thinking".to_string());
            }
            if model.get("attachment").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                capabilities.push("image_in".to_string());
            }
            if !capabilities.is_empty() {
                table.insert("capabilities".into(), capabilities.into());
            }
            models.insert(format!("{name}/{id}"), toml::Value::Table(table));
        }
    }
    if providers.is_empty() {
        bail!("opencode's config has no provider with an API key and models to import");
    }

    let configured = json_str(&config, "model");
    let default = configured.as_ref().filter(|alias| models.contains_key(*alias)).or_else(|| models.keys().next()).cloned();
    if let Some(alias) = &default {
        out.insert("default_model".into(), alias.clone().into());
        if configured.as_deref() != Some(alias.as_str()) {
            eprintln!("opencode's default model {} wasn't imported, so `{alias}` is the default", configured.as_deref().unwrap_or("(none)"));
        }
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, None, dst_dir, force)?;
    copy_opencode_mcp(&config, dst_dir, force)?;
    println!("Imported {provider_count} providers and {model_count} models into {}", dst_dir.join("config.toml").display());
    if !skipped.is_empty() {
        println!("Skipped providers without an API key (opencode OAuth, or no models): {}", skipped.join(", "));
    }
    Ok(())
}

/// The key opencode keeps for a provider: the config's `options.apiKey`, else the API key
/// `/connect` stored in `auth.json`. An OAuth sign-in has no key to copy.
fn opencode_key(provider: &serde_json::Value, auth: &serde_json::Value, name: &str) -> Option<String> {
    if let Some(key) = provider.get("options").and_then(|options| json_str(options, "apiKey")) {
        return Some(key);
    }
    let entry = auth.get(name)?;
    (entry.get("type").and_then(serde_json::Value::as_str) == Some("api")).then(|| json_str(entry, "key"))?
}

/// The npm package opencode names for a provider, as killi's provider type.
fn opencode_protocol(npm: Option<&str>) -> &'static str {
    match npm {
        Some("@ai-sdk/anthropic") => "anthropic",
        Some("@ai-sdk/openai") => "openai",
        _ => "openai",
    }
}

/// Writes opencode's MCP servers in killi's `mcpServers` shape: a local server's command
/// array becomes the command and its args, a remote one its URL. Disabled ones are left out.
fn copy_opencode_mcp(config: &serde_json::Value, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(all) = config.get("mcp").and_then(serde_json::Value::as_object).filter(|servers| !servers.is_empty()) else {
        return Ok(());
    };
    let dst = dst_dir.join("mcp.json");
    if dst.exists() && !force {
        return Ok(());
    }
    let mut servers = serde_json::Map::new();
    for (name, server) in all {
        if server.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
            continue;
        }
        let mut entry = serde_json::Map::new();
        let command = server.get("command").and_then(serde_json::Value::as_array);
        if let Some(command) = command.filter(|command| !command.is_empty()) {
            let args: Vec<&str> = command.iter().skip(1).filter_map(serde_json::Value::as_str).collect();
            entry.insert("command".into(), command[0].clone());
            if !args.is_empty() {
                entry.insert("args".into(), args.into());
            }
        } else if let Some(url) = json_str(server, "url") {
            entry.insert("url".into(), url.into());
        }
        if !entry.is_empty() {
            servers.insert(name.clone(), entry.into());
        }
    }
    if servers.is_empty() {
        return Ok(());
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dst_dir)?;
    let file = serde_json::json!({ "mcpServers": servers });
    fs::write(&dst, serde_json::to_string_pretty(&file)?).with_context(|| format!("could not write {}", dst.display()))?;
    println!("Copied MCP servers into {}", dst.display());
    Ok(())
}

/// Reads a JSON file, or `None` when there isn't one.
fn read_json(path: &Path) -> Result<Option<serde_json::Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("could not read {}", path.display())),
    };
    Ok(Some(serde_json::from_str(&text).with_context(|| format!("invalid JSON in {}", path.display()))?))
}

/// A JSON object's string field, when it is set and not blank.
fn json_str(value: &serde_json::Value, field: &str) -> Option<String> {
    value.get(field).and_then(serde_json::Value::as_str).map(String::from).filter(|text| !text.is_empty())
}

/// A JSON object's number field.
fn json_u64(value: &serde_json::Value, field: &str) -> Option<u64> {
    value.get(field).and_then(serde_json::Value::as_u64)
}
