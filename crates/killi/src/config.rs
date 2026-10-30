mod command;
mod edit;
mod guide;
mod import;
mod models;
mod muse;
mod resolve;
mod yaml;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

pub use command::command_guide_if_first;
pub use edit::{remove_provider_key, set_backup_model, set_model_effort, set_search};
pub use guide::{guide_model, init, set_up_if_missing};
pub use import::{import_kimi, import_opencode, import_omp, import_pi};
pub use models::{Added, Additions, NewModel, add_models};
pub use muse::{add as add_muse, ours as muse_ours};
pub(crate) use command::{confirm, read_line, read_secret};

use edit::{set_default_model, set_provider_key};
use guide::prompt_section;
use models::toml_int;
use yaml::read_yaml;

#[cfg(test)]
use command::{agree, create_command_link, valid_command_name};
#[cfg(test)]
use guide::TEMPLATE;
#[cfg(test)]
use import::{import_opencode_from, import_omp_from, import_pi_from};
#[cfg(test)]
use models::with_models;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub default_model: Option<String>,
    /// The model every session falls back to when its own exhausts its retries on a rate
    /// limit; `/backup-model` sets it.
    pub backup_model: Option<String>,
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
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub search: SearchConfig,
}

/// `[search]`: the service behind `web_search`. `web_search` tries that service first,
/// retrying a transient failure once, then any other source the table already makes
/// usable — a `url` instance needs no key of its own here — and Brave's public search
/// page last, which needs nothing and can turn automated requests away.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SearchConfig {
    /// "brave" (default), "tavily" or "searxng".
    pub provider: Option<String>,
    /// The SearXNG instance, such as "http://localhost:8888"; only "searxng" reads it.
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
}

impl SearchConfig {
    /// What `web_search` reads from it: the provider, its key — `api_key`, or the variable
    /// `api_key_env` names — and the SearXNG instance.
    pub fn settings(&self) -> crate::tools::SearchSettings {
        crate::tools::SearchSettings {
            provider: self.provider.clone().unwrap_or_default(),
            url: self.url.clone(),
            api_key: self
                .api_key
                .clone()
                .or_else(|| self.api_key_env.as_deref().and_then(|name| std::env::var(name).ok()))
                .filter(|key| !key.is_empty()),
        }
    }
}

/// `[ui]`: how the terminal UI looks.
#[derive(Debug, Default, Deserialize)]
pub struct UiConfig {
    /// "dark" or "light", for the terminal's background. Without it killi reads `COLORFGBG`
    /// and assumes dark when that isn't set.
    pub theme: Option<String>,
    /// Take the whole screen: the prompt and status line stay on the bottom rows, only the
    /// conversation scrolls (wheel, PageUp, PageDown) and it is laid out again when the window
    /// changes size. Without it that is so in Kil only; other terminals keep the conversation
    /// in their own scrollback.
    pub fullscreen: Option<bool>,
    /// A 100-character recap of the session after each turn that used tools (on unless
    /// `false`). `/recap` asks for one at any time either way.
    pub recap: Option<bool>,
}

/// `[prompt]`: the system prompt's guidance, kept in the config so it can be read and edited.
/// Without it killi uses its built-in text.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct PromptConfig {
    pub system: Option<String>,
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
    /// Subagents an `agent_swarm` call runs at once (default 8).
    pub swarm_parallel: Option<usize>,
    /// Start every session in swarm mode (default true); `/swarm on|off` still changes it.
    pub swarm: Option<bool>,
    /// Let the other killi sessions on this machine see this one, ask it about its work and
    /// leave it messages, and tell the model about those working in its tree (default true).
    pub peers: Option<bool>,
}

impl AgentConfig {
    /// Whether a session starts in swarm mode.
    pub fn swarm_on(&self) -> bool {
        self.swarm.unwrap_or(true)
    }

    /// Whether a session joins the live sessions (`live.rs`).
    pub fn peers_on(&self) -> bool {
        self.peers.unwrap_or(true)
    }
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
    /// "openai" (Chat Completions), "anthropic" (Messages), "openai_responses" (Responses),
    /// or "chatgpt": Responses on the user's ChatGPT plan, signed in with `killi login`.
    #[serde(rename = "type")]
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub custom_headers: BTreeMap<String, String>,
    /// Tool names the API refuses, as `killi's name = "the name sent and read back"`. Meta's
    /// API, for one, serves a Muse Code subscription only without another client's tool names,
    /// so the Responses API carries the mapped names both ways.
    #[serde(default)]
    pub tool_names: BTreeMap<String, String>,
    /// A header that carries the conversation's ID on every request, for a gateway that routes
    /// and caches by it (OpenCode Go reads `x-opencode-session`).
    pub session_header: Option<String>,
}

impl Provider {
    /// The key to send with requests: the environment variable's value when the provider
    /// reads one, else the inline `api_key`. `None` when it is unset or blank.
    fn key(&self) -> Option<String> {
        match (&self.api_key, &self.api_key_env) {
            (_, Some(var)) => std::env::var(var).ok(),
            (Some(key), None) => Some(key.clone()),
            (None, None) => None,
        }
        .filter(|key| !key.is_empty())
    }
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
    /// Overrides the provider's `base_url`, for a gateway that serves each API at its own URL.
    pub base_url: Option<String>,
    /// Mark the system message and the newest message with `cache_control` on Chat Completions,
    /// for gateways that pass it to a model with explicit prompt caching (Claude on Bedrock).
    #[serde(default)]
    pub cache_control: bool,
    pub default_effort: Option<String>,
    #[serde(default)]
    pub support_efforts: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ThinkingConfig {
    /// Turns reasoning off everywhere; otherwise each model reasons at its own
    /// `default_effort`, else the middle of its `support_efforts`.
    pub enabled: Option<bool>,
}

/// Everything a request needs, resolved from a model alias.
#[derive(Clone)]
pub struct ResolvedModel {
    pub alias: String,
    pub model: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: String,
    pub headers: BTreeMap<String, String>,
    /// The provider's `tool_names`, inverted as each reply is read: the name the API sent
    /// back to the killi name the session and its history keep.
    pub tool_names: BTreeMap<String, String>,
    /// The provider's `session_header`, sent with the session's ID.
    pub session_header: Option<String>,
    pub max_output: Option<u64>,
    pub context_window: Option<u64>,
    pub images: bool,
    pub thinking: Thinking,
    /// Chat Completions requests carry `cache_control` breakpoints.
    pub cache_control: bool,
    /// On the user's ChatGPT plan: requests carry the token `killi login` saved, refreshed as
    /// it expires, in place of `api_key`.
    pub chatgpt: bool,
}

/// The key stays out of logs and error reports: presence and length only, never the value.
impl std::fmt::Debug for ResolvedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = if self.api_key.is_empty() {
            "unset".to_string()
        } else {
            format!("<redacted {} chars>", self.api_key.len())
        };
        f.debug_struct("ResolvedModel")
            .field("alias", &self.alias)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("api_key", &key)
            .field("headers", &self.headers)
            .field("tool_names", &self.tool_names)
            .field("session_header", &self.session_header)
            .field("max_output", &self.max_output)
            .field("context_window", &self.context_window)
            .field("images", &self.images)
            .field("thinking", &self.thinking)
            .field("cache_control", &self.cache_control)
            .field("chatgpt", &self.chatgpt)
            .finish()
    }
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

/// The model killi would start on when its provider has no key.
pub struct MissingKey {
    /// The model alias that can't start.
    pub alias: String,
    /// The provider that names no usable key.
    pub provider: String,
}

/// Why a provider has no key: "$VAR isn't set", "api_key is blank" or "no api_key or api_key_env".
fn key_reason(provider: &Provider) -> String {
    match (&provider.api_key, &provider.api_key_env) {
        (_, Some(var)) => format!("${var} isn't set"),
        (Some(_), None) => "api_key is blank".to_string(),
        (None, None) => "no api_key or api_key_env".to_string(),
    }
}

/// Why the model killi would start on can't, without the advice `no_key` adds.
fn no_key_yet(alias: &str, provider: &Provider) -> String {
    format!("model `{alias}` needs a key, but {}", key_reason(provider))
}

/// What to do about a model that has no key, as `resolve` and a declined guide end on.
fn no_key(alias: &str, name: &str, provider: &Provider) -> String {
    let advice = match provider.api_key_env {
        Some(_) => format!("export it, or set api_key under [providers.{name}] in ~/.killi/config.toml, or start with -m <provider/model>"),
        None => format!("set api_key under [providers.{name}] in ~/.killi/config.toml"),
    };
    format!("{}: {advice}", no_key_yet(alias, provider))
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

#[cfg(test)]
mod tests;
