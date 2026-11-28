mod anthropic;
mod command;
mod custom;
mod edit;
mod gemini;
mod grok;
mod guide;
mod import;
mod kimi;
mod mimo;
mod minimax;
mod models;
mod muse;
mod qwen;
mod resolve;
mod stepfun;
mod unisound;
mod yaml;
mod zai;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;

pub use anthropic::{ABOUT as ANTHROPIC_ABOUT, add as add_anthropic, ours as anthropic_ours};
pub use command::command_guide_if_first;
pub(crate) use command::{confirm, read_line, read_secret, select};
pub use custom::{
    ABOUT as CUSTOM_ABOUT, add as add_custom, name as custom_name, served as custom_served,
    suggested_name as custom_suggested_name,
};
pub use edit::{
    remove_models, remove_provider_key, set_advisor_model, set_backup_model, set_mode,
    set_model_effort, set_search, set_swarm, set_swarm_parallel, set_ui_theme, set_verbosity,
};
pub use gemini::{ABOUT as GEMINI_ABOUT, add as add_gemini, ours as gemini_ours};
pub use grok::{
    ABOUT as GROK_ABOUT, BASE_URL as GROK_BASE_URL, add as add_grok, models as grok_models,
    ours as grok_ours,
};
pub use guide::{init, start_if_missing};
pub use import::import_command;
pub use kimi::{ABOUT as KIMI_ABOUT, add as add_kimi, ours as kimi_ours};
pub use mimo::{ABOUT as MIMO_ABOUT, add as add_mimo, ours as mimo_ours};
pub use minimax::{ABOUT as MINIMAX_ABOUT, add as add_minimax, ours as minimax_ours};
pub(crate) use models::kept_models;
pub use models::{Added, Additions, NewModel, add_models};
pub use muse::{ABOUT as MUSE_ABOUT, add as add_muse, ours as muse_ours};
pub use qwen::{ABOUT as QWEN_ABOUT, add as add_qwen, ours as qwen_ours};
pub use stepfun::{ABOUT as STEPFUN_ABOUT, add as add_stepfun, ours as stepfun_ours};
pub use unisound::{ABOUT as UNISOUND_ABOUT, add as add_unisound, ours as unisound_ours};
pub use zai::{ABOUT as ZAI_ABOUT, add as add_zai, ours as zai_ours};

use edit::{KEY_FIELDS, set_default_model, with_provider_fields};
use guide::prompt_section;
use models::toml_int;
use yaml::read_yaml;

#[cfg(test)]
use command::{agree, create_command_link, rows, valid_command_name};
#[cfg(test)]
use guide::{TEMPLATE, starting_config};
#[cfg(test)]
use import::{
    import_claude_from, import_codex_from, import_gemini_from, import_omp_from,
    import_opencode_from, import_pi_from,
};
#[cfg(test)]
use models::{with_additions, with_models};

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub default_model: Option<String>,
    /// The model every session falls back to when its own exhausts its retries on a rate
    /// limit; `/backup-model` sets it.
    pub backup_model: Option<String>,
    /// The model the session's model consults through the `advisor` tool, usually the
    /// strongest one configured; `/advisor` sets it, and every session started after it has the tool.
    pub advisor_model: Option<String>,
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
    /// `[models_filter.<provider>]`: which of a provider's models are listed, synced and
    /// started. A provider without an entry keeps every model.
    #[serde(default)]
    pub models_filter: ModelsFilter,
}

/// The per-provider model filters, keyed by the provider part of an alias.
pub type ModelsFilter = BTreeMap<String, ProviderFilter>;

/// One provider's `include`/`exclude` patterns: a full alias or a model id, and a trailing
/// `*` matches a prefix. An empty `include` keeps every model; `exclude` always wins.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ProviderFilter {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl ProviderFilter {
    /// Whether a model alias passes: not excluded, and included when `include` names any.
    pub fn allows(&self, alias: &str) -> bool {
        let id = alias.split_once('/').map_or(alias, |(_, id)| id);
        !self
            .exclude
            .iter()
            .any(|pattern| matches(pattern, alias, id))
            && (self.include.is_empty()
                || self
                    .include
                    .iter()
                    .any(|pattern| matches(pattern, alias, id)))
    }
}

/// Whether `pattern` names `alias` or its model id; a trailing `*` matches a prefix.
fn matches(pattern: &str, alias: &str, id: &str) -> bool {
    [alias, id]
        .iter()
        .any(|value| match pattern.strip_suffix('*') {
            Some(prefix) => value.starts_with(prefix),
            None => pattern == *value,
        })
}

/// `[search]`: the service behind `web_search`. `web_search` tries that service first,
/// retrying a transient failure once, then any other source the table already makes
/// usable — a `url` instance needs no key of its own here — and Brave's public search
/// page last, which needs nothing and can turn automated requests away.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SearchConfig {
    /// "brave" (default), "tavily", "keenable" or "searxng".
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
                .or_else(|| {
                    self.api_key_env
                        .as_deref()
                        .and_then(|name| std::env::var(name).ok())
                })
                .filter(|key| !key.is_empty()),
        }
    }
}

/// `[ui]`: how the terminal UI looks.
#[derive(Debug, Default, Deserialize)]
pub struct UiConfig {
    /// The color theme, as `/theme` picks it: "default" or a named skin `/theme` lists
    /// ("batman", "snow-and-ice", …), or "dark"/"light" to pin the default's shades to a terminal
    /// background. Without it the default reads `COLORFGBG` and assumes dark when that isn't
    /// set. The named themes are made for a dark terminal.
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
    /// Compact the conversation once a request's input passes this many tokens (default: 60% of
    /// the model's context window, 256000 when the window is unknown; 0 turns it off).
    pub compact_at: Option<u64>,
    /// Extra command prefixes bash may run in plan mode, such as "make -n".
    #[serde(default)]
    pub plan_allow: Vec<String>,
    /// Seconds a turn must take before it notifies (default 10).
    pub notify_after: Option<u64>,
    /// Whether killi notifies at all (default true): the terminal's notification or bell for
    /// turn ends, questions and messages from other sessions, and the OSC 7501 status reports.
    pub notify: Option<bool>,
    /// Subagents an `agent_swarm` call runs at once (default 8).
    pub swarm_parallel: Option<usize>,
    /// Minutes a subagent may run before it is stopped and reported as timed out (default 30,
    /// 0 for no limit).
    pub subagent_timeout: Option<u64>,
    /// Start every session in swarm mode (default true); `/swarm on|off` still changes it.
    pub swarm: Option<bool>,
    /// How long replies run: `normal` (default) or `ultra-concise`; `/verbosity` sets it.
    pub verbosity: Option<crate::verbosity::Verbosity>,
    /// How the session uses subagents: `focus` (default) or `orchestrate`; `/mode` sets it.
    /// This line used to hold the verbosity, and those values still read as one.
    pub mode: Option<ModeLine>,
    /// Let the other killi sessions on this machine see this one, ask it about its work and
    /// leave it messages, and tell the model about those working in its tree (default true).
    pub peers: Option<bool>,
    /// Requests one turn may make when no one watches it: `-p`, subagents, swarm members and
    /// tasks other sessions send (default 200). At the limit the agent is asked to reply with
    /// where the work stands, and the turn ends. 0 turns the limit off.
    pub max_steps: Option<u32>,
    /// The same for a turn the user watches in the terminal (default none; 0 is none too).
    pub max_steps_interactive: Option<u32>,
}

/// Requests an unattended turn makes before it wraps up, unless `[agent] max_steps` says.
pub const DEFAULT_MAX_STEPS: u32 = 200;

impl AgentConfig {
    /// The step limit for a turn no one watches; `None` when turned off.
    pub fn max_steps(&self) -> Option<u32> {
        Some(self.max_steps.unwrap_or(DEFAULT_MAX_STEPS)).filter(|n| *n > 0)
    }

    /// The step limit for a turn the user watches; `None` unless one is set.
    pub fn max_steps_interactive(&self) -> Option<u32> {
        self.max_steps_interactive.filter(|n| *n > 0)
    }

    /// Whether a session starts in swarm mode.
    pub fn swarm_on(&self) -> bool {
        self.swarm.unwrap_or(true)
    }

    /// Whether a session joins the live sessions (`live.rs`).
    pub fn peers_on(&self) -> bool {
        self.peers.unwrap_or(true)
    }

    /// The reply verbosity a session starts with: `verbosity`, else an old `mode` line that
    /// set it.
    pub fn verbosity(&self) -> crate::verbosity::Verbosity {
        match (self.verbosity, self.mode) {
            (Some(verbosity), _) => verbosity,
            (None, Some(ModeLine::Legacy(verbosity))) => verbosity,
            _ => Default::default(),
        }
    }

    /// The working mode a session starts in; an old verbosity value in `mode` leaves the default.
    pub fn mode(&self) -> crate::mode::Mode {
        match self.mode {
            Some(ModeLine::Work(mode)) => mode,
            _ => Default::default(),
        }
    }
}

/// What `[agent] mode` holds: a working mode, or a reply verbosity written before `/verbosity`
/// took that over. The names don't overlap, so each value means one thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeLine {
    Work(crate::mode::Mode),
    Legacy(crate::verbosity::Verbosity),
}

impl<'de> Deserialize<'de> for ModeLine {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        if let Some(mode) = crate::mode::Mode::parse(&name) {
            return Ok(ModeLine::Work(mode));
        }
        if let Some(verbosity) = crate::verbosity::Verbosity::parse(&name) {
            return Ok(ModeLine::Legacy(verbosity));
        }
        Err(serde::de::Error::custom(format!(
            "unknown mode `{name}`, expected `focus` or `orchestrate`"
        )))
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
    /// "commandcode" (Command Code's subscription API), or a plan signed in with `killi
    /// login`: "chatgpt" (Responses on the user's ChatGPT plan) or "grok" (Responses on
    /// xAI's API with the user's Grok subscription).
    #[serde(rename = "type")]
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub custom_headers: BTreeMap<String, String>,
    /// Tool names the API refuses, as `killi's name = "the name sent and read back"`. Meta's
    /// API, for one, serves a Muse Code subscription only without another client's tool names,
    /// so requests carry the mapped names and replies are read back under killi's.
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
    /// The short name shown in the UI; `None` falls back to the alias.
    #[serde(default)]
    pub display_name: Option<String>,
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
    pub display_name: Option<String>,
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
    /// On a subscription `killi login` signed in to: requests carry its saved token, refreshed
    /// as it expires, in place of `api_key`.
    pub plan: Option<Plan>,
}

impl ResolvedModel {
    /// A session with no model yet: an interactive start opens on `/provider`, and the first
    /// request says so.
    pub fn unconfigured() -> Self {
        Self {
            alias: String::new(),
            model: String::new(),
            display_name: None,
            protocol: Protocol::OpenAi,
            base_url: String::new(),
            api_key: String::new(),
            headers: Default::default(),
            tool_names: Default::default(),
            session_header: None,
            max_output: None,
            context_window: None,
            images: false,
            thinking: Thinking::Off,
            cache_control: false,
            plan: None,
        }
    }
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
            .field("display_name", &self.display_name)
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
            .field("plan", &self.plan)
            .finish()
    }
}

/// A subscription killi signs in to with `killi login`, whose provider `type` names it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Plan {
    /// `type = "chatgpt"`: Responses on the user's ChatGPT plan.
    ChatGpt,
    /// `type = "grok"`: Responses on xAI's API with the user's SuperGrok or X Premium plan.
    Grok,
}

impl Plan {
    /// The plan a provider `type` names, if any.
    pub fn of(kind: &str) -> Option<Plan> {
        match kind {
            "chatgpt" => Some(Plan::ChatGpt),
            "grok" => Some(Plan::Grok),
            _ => None,
        }
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
    /// Command Code's subscription API.
    CommandCode,
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
        Some(_) => format!(
            "export it, or set api_key under [providers.{name}] in ~/.killi/config.toml, or start with -m <provider/model>"
        ),
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

/// Refreshes the model lists that come from a provider's own API, using the keys the config
/// already has: OpenCode Go's account list and Z.ai's `/models`. With `only`, just the
/// provider of that name. Each line reports what changed, or why it couldn't.
pub async fn sync_models(only: Option<&str>) -> Result<Vec<String>> {
    let config = Config::load()?;
    let mut out = Vec::new();
    for (name, provider) in &config.providers {
        if only.is_some_and(|want| want != name) {
            continue;
        }
        let Some(key) = provider.key() else {
            continue;
        };
        let Some(base) = provider.base_url.as_deref() else {
            continue;
        };
        let inline = provider.api_key.is_some() && provider.api_key_env.is_none();
        let synced = if base.starts_with("https://opencode.ai/inference/go/") {
            let result = match (inline, provider.api_key_env.as_deref()) {
                (true, _) => crate::login::opencode_go_add(Some(&key)).await,
                (false, Some(env)) => crate::login::opencode_go_add_env(env, &key).await,
                (false, None) => crate::login::opencode_go_add(None).await,
            };
            result.map(|outcome| {
                out.extend(outcome.notes);
                outcome.added
            })
        } else if zai::ours(provider) {
            match (inline, provider.api_key_env.as_deref()) {
                (true, _) => zai::add(&key).await,
                (false, Some(env)) => zai::add_env(env, &key).await,
                (false, None) => continue,
            }
        } else {
            continue;
        };
        match synced {
            Ok(added) => {
                let count = Config::load()?
                    .models
                    .values()
                    .filter(|model| model.provider == added.provider)
                    .count();
                out.push(added.report());
                out.push(format!("{}: {count} models", added.provider));
            }
            Err(error) => out.push(format!("{name}: {error:#}")),
        }
    }
    if out.is_empty() {
        out.push("No OpenCode Go or Z.ai provider with a key to sync.".to_string());
    }
    Ok(out)
}

pub fn killi_home() -> PathBuf {
    home().join(".killi")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests;
