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
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub hooks: crate::hooks::HooksConfig,
    #[serde(default)]
    pub mcp: McpConfig,
}

/// `[mcp]`: MCP servers to start with every session, with native tool definitions. A project
/// can name its own in `.killi/config.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub eager: Vec<String>,
}

/// `[agent]`: how the agent behaves.
#[derive(Debug, Default, Deserialize)]
pub struct AgentConfig {
    /// Compact the conversation once a request's input passes this many tokens (default 256000).
    pub compact_at: Option<u64>,
    /// Extra command prefixes bash may run in plan mode, such as "make -n".
    #[serde(default)]
    pub plan_allow: Vec<String>,
    /// Seconds a turn must take before a desktop notification (default 10).
    pub notify_after: Option<u64>,
}

/// `[skills]`: where skills are found besides the project's own folders.
#[derive(Debug, Default, Deserialize)]
pub struct SkillsConfig {
    /// Defaults to `~/.agents/skills`.
    #[serde(default)]
    pub dirs: Vec<String>,
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
    pub max_context_size: Option<u64>,
    pub max_output_size: Option<u64>,
    #[serde(default)]
    pub capabilities: Vec<String>,
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
    pub context_window: Option<u64>,
    pub images: bool,
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
            format!("could not read {}; run `killi init` to create one, or `killi import-kimi` to copy Kimi Code's", path.display())
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
            context_window: model.max_context_size,
            images: model.capabilities.iter().any(|c| c == "image_in"),
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
        effort.as_deref().map_or(Thinking::On, Thinking::parse)
    }

    /// The efforts `/effort` offers for a model, as Kimi Code lists them: off, then the
    /// model's `support_efforts`, or on/off and the common levels when it declares none.
    pub fn efforts(&self, alias: &str) -> Vec<String> {
        let declared = self.models.get(alias).map(|m| m.support_efforts.clone()).unwrap_or_default();
        let levels: Vec<&str> = if declared.is_empty() { vec!["on", "low", "medium", "high"] } else { Vec::new() };
        std::iter::once("off").chain(levels).map(String::from).chain(declared).collect()
    }
}

impl Thinking {
    pub fn parse(effort: &str) -> Self {
        match effort {
            "on" => Thinking::On,
            "off" => Thinking::Off,
            effort => Thinking::Effort(effort.to_string()),
        }
    }

    /// The name `parse` reads back.
    pub fn label(&self) -> &str {
        match self {
            Thinking::Off => "off",
            Thinking::On => "on",
            Thinking::Effort(effort) => effort,
        }
    }
}

const TEMPLATE: &str = r#"# killi's config. Models are named "<provider>/<model>"; pick one with -m or default_model.
default_model = "openai/gpt-5.5"

[thinking]
effort = "high"                 # or: enabled = false

[providers.openai]
type = "openai"                 # OpenAI chat completions, and compatible APIs
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"  # or: api_key = "..."

[providers.anthropic]
type = "anthropic"              # Anthropic Messages, and compatible APIs
api_key_env = "ANTHROPIC_API_KEY"

[models."openai/gpt-5.5"]
provider = "openai"
model = "gpt-5.5"
protocol = "openai_responses"   # use the Responses API for this model
max_context_size = 400000
max_output_size = 128000
capabilities = ["image_in", "thinking", "tool_use"]

[models."anthropic/claude-sonnet-5-5"]
provider = "anthropic"
model = "claude-sonnet-5-5"
max_context_size = 1000000
max_output_size = 128000
capabilities = ["image_in", "thinking", "tool_use"]
"#;

/// Writes a starting `~/.killi/config.toml` to edit.
pub fn init() -> Result<()> {
    let dir = killi_home();
    let path = dir.join("config.toml");
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    file.write_all(TEMPLATE.as_bytes())?;
    println!("Wrote {}. Set your providers' keys and models there, then run `killi`.", path.display());
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

    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&dst)?;
    file.write_all(toml::to_string(&out)?.as_bytes())?;

    println!("Imported {provider_count} providers and {model_count} models into {}", dst.display());
    let mcp_src = home().join(".kimi-code/mcp.json");
    let mcp_dst = dir.join("mcp.json");
    if mcp_src.exists() && (!mcp_dst.exists() || force) {
        let text = fs::read(&mcp_src)?;
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&mcp_dst)?;
        file.write_all(&text)?;
        println!("Copied MCP servers into {}", mcp_dst.display());
    }
    if !skipped.is_empty() {
        println!("Skipped providers without an API key (Kimi account login): {}", skipped.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn template_parses_and_resolves() {
        let config: super::Config = toml::from_str(super::TEMPLATE).unwrap();
        assert_eq!(config.models.len(), 2);
        // SAFETY: tests that read this variable don't run concurrently with one that changes it.
        unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, super::Protocol::Responses);
        assert_eq!(model.thinking, super::Thinking::Effort("high".into()));
    }
}
