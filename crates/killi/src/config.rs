use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub models: BTreeMap<String, Model>,
    #[serde(default)]
    pub thinking: ThinkingConfig,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    #[serde(rename = "type")]
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub custom_headers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub provider: String,
    pub model: String,
    pub max_output_size: Option<u64>,
    pub protocol: Option<String>,
    pub default_effort: Option<String>,
    #[serde(default)]
    pub support_efforts: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: Option<bool>,
    pub effort: Option<String>,
}

/// Everything a request needs, resolved from a model alias.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub alias: String,
    pub model: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: String,
    pub headers: BTreeMap<String, String>,
    pub max_output: Option<u64>,
    pub thinking: Thinking,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    /// OpenAI Chat Completions.
    OpenAi,
    /// Anthropic Messages.
    Anthropic,
    /// OpenAI Responses.
    Responses,
}

/// How much the model should reason, resolved the way Kimi Code does it.
#[derive(Debug, Clone, PartialEq)]
pub enum Thinking {
    Off,
    /// On at the provider's default effort.
    On,
    Effort(String),
}

pub fn killi_home() -> PathBuf {
    home().join(".killi")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = killi_home().join("config.toml");
        let text = fs::read_to_string(&path).with_context(|| {
            format!("could not read {}; run `killi import-kimi` to create it", path.display())
        })?;
        toml::from_str(&text).with_context(|| format!("invalid config in {}", path.display()))
    }

    pub fn resolve(&self, requested: Option<&str>) -> Result<ResolvedModel> {
        let Some(alias) = requested.or(self.default_model.as_deref()) else {
            bail!("no model chosen: pass -m <provider/model> or set default_model in ~/.killi/config.toml");
        };
        let Some(model) = self.models.get(alias) else {
            bail!("unknown model `{alias}`; models are listed under [models.\"...\"] in ~/.killi/config.toml");
        };
        let Some(provider) = self.providers.get(&model.provider) else {
            bail!("model `{alias}` names provider `{}`, which isn't configured", model.provider);
        };
        let protocol = match model.protocol.as_deref().unwrap_or(&provider.kind) {
            "openai" => Protocol::OpenAi,
            "anthropic" => Protocol::Anthropic,
            "openai_responses" => Protocol::Responses,
            other => bail!(
                "model `{alias}` uses the `{other}` API, which killi doesn't support; use openai, anthropic or openai_responses"
            ),
        };
        let base_url = match (&provider.base_url, protocol) {
            (Some(url), _) => url.clone(),
            (None, Protocol::Anthropic) => "https://api.anthropic.com".to_string(),
            (None, _) => "https://api.openai.com/v1".to_string(),
        };
        let api_key = match (&provider.api_key, &provider.api_key_env) {
            (_, Some(var)) => std::env::var(var)
                .with_context(|| format!("provider `{}` reads its key from ${var}, which isn't set", model.provider))?,
            (Some(key), None) => key.clone(),
            (None, None) => bail!("provider `{}` has neither api_key nor api_key_env", model.provider),
        };
        Ok(ResolvedModel {
            alias: alias.to_string(),
            model: model.model.clone(),
            protocol,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            headers: provider.custom_headers.clone(),
            max_output: model.max_output_size,
            thinking: self.thinking_for(model),
        })
    }

    /// Kimi Code's order: `thinking.enabled = false` turns reasoning off, then the configured
    /// effort, then the model's default effort, then the middle of its supported efforts.
    fn thinking_for(&self, model: &Model) -> Thinking {
        if self.thinking.enabled == Some(false) {
            return Thinking::Off;
        }
        let effort = self.thinking.effort.clone().or_else(|| model.default_effort.clone()).or_else(|| {
            let efforts = &model.support_efforts;
            (!efforts.is_empty()).then(|| efforts[efforts.len() / 2].clone())
        });
        match effort.as_deref() {
            None | Some("on") => Thinking::On,
            Some("off") => Thinking::Off,
            Some(effort) => Thinking::Effort(effort.to_string()),
        }
    }
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

    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&dst)?;
    file.write_all(toml::to_string(&out)?.as_bytes())?;

    println!("Imported {provider_count} providers and {model_count} models into {}", dst.display());
    if !skipped.is_empty() {
        println!("Skipped providers without an API key (Kimi account login): {}", skipped.join(", "));
    }
    Ok(())
}
