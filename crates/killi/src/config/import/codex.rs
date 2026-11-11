use super::*;
use std::path::Path;

use anyhow::{Result, bail};

/// Copies custom providers, the configured model and MCP servers from Codex CLI's config
/// (`~/.codex/config.toml`). Each `[model_providers.<id>]` with an endpoint and an `env_key`
/// becomes a provider reading that variable; Codex's own account in `auth.json` is its
/// login and is never imported, so a model on it falls back to the first custom provider.
pub fn import_codex(force: bool) -> Result<()> {
    import_codex_from(&home().join(".codex/config.toml"), &killi_home(), force)
}

pub(crate) fn import_codex_from(config_path: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(config) = read_toml(config_path)? else {
        bail!("no codex config at {} to import", config_path.display())
    };

    let mut providers = toml::Table::new();
    let mut skipped = Vec::new();
    let mut dropped_params = false;
    if let Some(toml::Value::Table(all)) = config.get("model_providers") {
        for (name, provider) in all {
            let base_url = provider
                .get("base_url")
                .and_then(toml::Value::as_str)
                .filter(|url| !url.is_empty());
            let env_key = provider
                .get("env_key")
                .and_then(toml::Value::as_str)
                .filter(|key| !key.is_empty());
            let (Some(base_url), Some(env_key)) = (base_url, env_key) else {
                skipped.push(name.clone());
                continue;
            };
            let mut table = toml::Table::new();
            let wire = provider.get("wire_api").and_then(toml::Value::as_str);
            table.insert("type".into(), codex_protocol(wire).into());
            table.insert("base_url".into(), base_url.into());
            table.insert("api_key_env".into(), env_key.into());
            if let Some(toml::Value::Table(headers)) = provider.get("http_headers") {
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
            dropped_params |= provider
                .get("query_params")
                .and_then(toml::Value::as_table)
                .is_some_and(|params| !params.is_empty());
            providers.insert(name.clone(), toml::Value::Table(table));
        }
    }
    if providers.is_empty() {
        bail!(
            "codex's config has no custom provider with an endpoint and key to import (its own account login isn't importable)"
        );
    }

    let mut out = toml::Table::new();
    let mut models = toml::Table::new();
    let picked = config
        .get("model")
        .and_then(toml::Value::as_str)
        .filter(|id| !id.is_empty());
    match picked {
        Some(id) => {
            let selected = config
                .get("model_provider")
                .and_then(toml::Value::as_str)
                .filter(|name| !name.is_empty());
            let name = match selected.filter(|name| providers.contains_key(*name)) {
                Some(name) => name.to_string(),
                None => {
                    let fallback = providers.keys().next().cloned().unwrap_or_default();
                    if let Some(missed) = selected {
                        eprintln!(
                            "codex's model provider `{missed}` wasn't imported (its own account login isn't), so `{fallback}` is used"
                        );
                    }
                    fallback
                }
            };
            let mut table = toml::Table::new();
            table.insert("provider".into(), name.clone().into());
            table.insert("model".into(), id.into());
            if let Some(context) = config
                .get("model_context_window")
                .and_then(toml::Value::as_integer)
                .and_then(|size| u64::try_from(size).ok())
            {
                table.insert("max_context_size".into(), toml_int(context));
            }
            if let Some(effort) = config
                .get("model_reasoning_effort")
                .and_then(toml::Value::as_str)
                .filter(|effort| !effort.is_empty())
            {
                table.insert("default_effort".into(), effort.into());
            }
            let alias = format!("{name}/{id}");
            models.insert(alias.clone(), toml::Value::Table(table));
            out.insert("default_model".into(), alias.into());
        }
        None => eprintln!("codex's config names no model, so none is the default"),
    }
    if dropped_params {
        eprintln!("codex's query_params have nowhere to go in killi's config and were dropped");
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, None, dst_dir, force)?;
    copy_codex_mcp(&config, dst_dir, force)?;
    println!(
        "Imported {provider_count} providers and {model_count} models into {}",
        dst_dir.join("config.toml").display()
    );
    if !skipped.is_empty() {
        println!(
            "Skipped providers without an endpoint or key variable: {}",
            skipped.join(", ")
        );
    }
    Ok(())
}

/// Codex's `wire_api` as killi's provider type: `responses` speaks the Responses API,
/// anything else (usually `chat`) the chat one.
fn codex_protocol(wire: Option<&str>) -> &'static str {
    match wire {
        Some("responses") => "openai_responses",
        _ => "openai",
    }
}

/// Writes Codex's `[mcp_servers.*]` in killi's shape: stdio servers keep their command,
/// args and environment, remote ones their URL, headers and bearer variable. Headers that
/// name an environment variable are read now; unset ones are left out and reported.
fn copy_codex_mcp(config: &toml::Table, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(toml::Value::Table(all)) = config.get("mcp_servers") else {
        return Ok(());
    };
    let mut servers = serde_json::Map::new();
    let mut unset = Vec::new();
    for (name, server) in all {
        if server.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
            continue;
        }
        let mut entry = serde_json::Map::new();
        if let Some(command) = server.get("command").and_then(toml::Value::as_str) {
            entry.insert("command".into(), command.into());
            let args: Vec<&str> = server
                .get("args")
                .and_then(toml::Value::as_array)
                .map(|args| args.iter().filter_map(toml::Value::as_str).collect())
                .unwrap_or_default();
            if !args.is_empty() {
                entry.insert("args".into(), args.into());
            }
            let mut env = serde_json::Map::new();
            for field in ["env", "env_vars"] {
                if let Some(toml::Value::Table(vars)) = server.get(field) {
                    for (key, value) in vars {
                        if let Some(value) = value.as_str() {
                            env.insert(key.clone(), value.into());
                        }
                    }
                }
            }
            if !env.is_empty() {
                entry.insert("env".into(), env.into());
            }
        } else if let Some(url) = server.get("url").and_then(toml::Value::as_str) {
            entry.insert("url".into(), url.into());
            let mut headers = serde_json::Map::new();
            if let Some(toml::Value::Table(fixed)) = server.get("http_headers") {
                for (header, value) in fixed {
                    if let Some(value) = value.as_str() {
                        headers.insert(header.clone(), value.into());
                    }
                }
            }
            if let Some(toml::Value::Table(named)) = server.get("env_http_headers") {
                for (header, var) in named {
                    match var
                        .as_str()
                        .and_then(|var| std::env::var(var).ok())
                        .filter(|value| !value.is_empty())
                    {
                        Some(value) => {
                            headers.insert(header.clone(), value.into());
                        }
                        None => unset.push(format!("{name}/{header}")),
                    }
                }
            }
            if !headers.is_empty() {
                entry.insert("headers".into(), headers.into());
            }
            if let Some(var) = server
                .get("bearer_token_env_var")
                .and_then(toml::Value::as_str)
            {
                // killi's MCP file is camelCase, like Claude Code's.
                entry.insert("bearerTokenEnvVar".into(), var.into());
            }
        }
        if !entry.is_empty() {
            servers.insert(name.clone(), entry.into());
        }
    }
    write_mcp_json(&servers, dst_dir, force)?;
    if !unset.is_empty() {
        eprintln!(
            "Left out headers whose variable isn't set: {}",
            unset.join(", ")
        );
    }
    Ok(())
}
