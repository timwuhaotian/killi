use std::collections::BTreeMap;
use std::fs;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub prompt: PromptConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub search: SearchConfig,
}

/// `[search]`: the service behind `web_search`. Without a key, "brave" reads Brave's public
/// search page, which can turn automated requests away.
#[derive(Debug, Default, Deserialize)]
pub struct SearchConfig {
    /// "brave" (default) or "tavily".
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
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

impl Config {
    pub fn load() -> Result<Self> {
        let path = killi_home().join("config.toml");
        let text = match fs::read_to_string(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
                "no config yet at {}; run `killi init` to write one to edit, or `killi import-kimi` to copy Kimi Code's",
                path.display()
            ),
            read => read.with_context(|| format!("could not read {}", path.display()))?,
        };
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
        let Some(api_key) = provider.key() else { bail!("{}", no_key(alias, &model.provider, provider)) };
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

    /// The model killi would start on when its provider has no key: the alias and the
    /// provider's name. `None` when that model has a key, or when something else is wrong
    /// with it, which `resolve` reports.
    pub fn missing_key(&self, requested: Option<&str>) -> Option<MissingKey> {
        let alias = requested.or(self.default_model.as_deref())?;
        let model = self.models.get(alias)?;
        let provider = self.providers.get(&model.provider)?;
        provider.key().is_none().then(|| MissingKey { alias: alias.to_string(), provider: model.provider.clone() })
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

    /// The system prompt's guidance: `[prompt] system`, or the built-in text.
    pub fn guidance(&self) -> &str {
        self.prompt.system.as_deref().filter(|text| !text.trim().is_empty()).unwrap_or(crate::prompt::DEFAULT)
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

/// The `[prompt]` table with the built-in guidance, as `init` and `import-kimi` write it.
fn prompt_section() -> String {
    format!(
        "\n# The system prompt's guidance, sent first in every session. Edit it here, then /reload or\n\
         # start a new session; the environment, AGENTS.md files and skill names follow it. While this\n\
         # table is here, killi updates don't change your prompt; delete it to use the built-in text.\n\
         [prompt]\nsystem = '''\n{}'''\n",
        crate::prompt::DEFAULT
    )
}

/// Writes a starting `~/.killi/config.toml` to edit, or adds the `[prompt]` table to an
/// existing one that has none.
pub fn init() -> Result<()> {
    let dir = killi_home();
    let path = dir.join("config.toml");
    if path.exists() {
        let text = fs::read_to_string(&path)?;
        let table: toml::Table = toml::from_str(&text).with_context(|| format!("invalid config in {}", path.display()))?;
        if table.contains_key("prompt") {
            bail!("{} already exists and has a [prompt] table", path.display());
        }
        let mut file = fs::OpenOptions::new().append(true).open(&path)?;
        if !text.is_empty() && !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        file.write_all(prompt_section().as_bytes())?;
        println!("Added the system prompt to {} under [prompt]; edit it there.", path.display());
        return Ok(());
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    file.write_all(TEMPLATE.as_bytes())?;
    file.write_all(prompt_section().as_bytes())?;
    println!("Wrote {}. Set your providers' keys and models there.", path.display());
    Ok(())
}

/// A coding agent whose config killi can import: its name, the config that shows it is
/// set up on this machine, and the importer for it.
type ImportSource = (&'static str, PathBuf, fn(bool) -> Result<()>);

/// The other coding agents whose config killi can import, that are set up on this machine.
fn import_sources() -> Vec<ImportSource> {
    let mut sources: Vec<ImportSource> = Vec::new();
    let kimi = home().join(".kimi-code/config.toml");
    if kimi.exists() {
        sources.push(("Kimi Code", kimi, import_kimi));
    }
    let pi = home().join(".pi/agent/models.json");
    if pi.exists() {
        sources.push(("pi", pi, import_pi));
    }
    let omp_dir = home().join(".omp/agent");
    if let Some(omp) = ["models.yml", "models.yaml", "models.json"].into_iter().map(|name| omp_dir.join(name)).find(|path| path.exists()) {
        sources.push(("omp", omp, import_omp));
    }
    let opencode = home().join(".config/opencode/opencode.json");
    if opencode.exists() {
        sources.push(("opencode", opencode, import_opencode));
    }
    sources
}

/// When killi starts in a terminal without a config, offers to make one from another coding
/// agent's — whichever are set up on this machine — or from the starting template. The start
/// then goes on to `guide_model`, which gives the new config a key; declining every offer
/// leaves `load` to explain that there is no config.
pub fn set_up_if_missing() -> Result<()> {
    let path = killi_home().join("config.toml");
    if path.exists() || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    eprintln!("killi has no config yet at {}.", path.display());
    for (name, source, import) in import_sources() {
        if confirm(&format!("Import providers, models and MCP servers from {name} ({})?", source.display()))? {
            if let Err(error) = import(false) {
                eprintln!("killi: {error:#}");
                continue;
            }
            return Ok(());
        }
    }
    if confirm("Write a starting config to edit?")? {
        init()?;
    }
    Ok(())
}

/// The first-run model guide: when the model killi would start on has no key, offers to
/// paste one — stored in the config file, read with echo off — or to start on another
/// configured model. Interactive starts only: `-p` and piped runs keep failing with the
/// reason from `resolve`, and so does a start where every offer is declined.
pub fn guide_model(model: Option<&str>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    let path = killi_home().join("config.toml");
    loop {
        let config = Config::load()?;
        let Some(missing) = config.missing_key(model) else { return Ok(()) };
        let Some(provider) = config.providers.get(&missing.provider) else { return Ok(()) };
        eprintln!("{}", no_key_yet(&missing.alias, provider));
        let otherwise = if model.is_some() { "press Enter to quit" } else { "press Enter to choose another model" };
        let key = read_secret(&format!("Paste the key to store it in ~/.killi/config.toml, or {otherwise}: "))?;
        if !key.is_empty() {
            set_provider_key(&path, &missing.provider, &key)?;
            continue;
        }
        if model.is_some() || !pick_model(&config, &path)? {
            return Ok(());
        }
    }
}

/// Lists the configured models with whether their key is there, and makes the chosen one the
/// default. False when the user keeps the model they have.
fn pick_model(config: &Config, path: &Path) -> Result<bool> {
    let aliases: Vec<&str> = config.models.keys().map(String::as_str).collect();
    eprintln!("Models in ~/.killi/config.toml:");
    for (index, alias) in aliases.iter().enumerate() {
        let note = match config.missing_key(Some(alias)) {
            Some(missing) => key_reason(&config.providers[&missing.provider]),
            None => "ready".to_string(),
        };
        eprintln!("  {}. {alias} — {note}", index + 1);
    }
    let answer = read_line("Start with which? ")?;
    match answer.parse::<usize>() {
        Ok(choice) if (1..=aliases.len()).contains(&choice) => {
            set_default_model(path, aliases[choice - 1])?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Stores `key` as the `api_key` of `provider` in the config file, in place of the
/// `api_key` or `api_key_env` line in its section, so `resolve` reads the key from the file.
/// The rest of the file, comments included, is left as it is.
fn set_provider_key(path: &Path, provider: &str, key: &str) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let header = format!("[providers.{provider}]");
    let key_line = format!("api_key = {}", toml::Value::String(key.to_string()));
    let mut out: Vec<String> = Vec::new();
    let mut in_provider = false;
    let mut written = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_provider = trimmed.starts_with(&header);
        } else if in_provider && trimmed.starts_with("api_key") {
            if !written {
                out.push(key_line.clone());
                written = true;
            }
            continue;
        }
        out.push(line.to_string());
    }
    if !written {
        let Some(at) = out.iter().position(|line| line.trim().starts_with(&header)) else {
            bail!("no {header} table in {} to store the key in", path.display())
        };
        out.insert(at + 1, key_line);
    }
    fs::write(path, format!("{}\n", out.join("\n"))).with_context(|| format!("could not write {}", path.display()))
}

/// Makes `alias` the `default_model` of the config file: the line is replaced where the file
/// has one before its first table, and added at the top where it has none.
fn set_default_model(path: &Path, alias: &str) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let line = format!("default_model = {}", toml::Value::String(alias.to_string()));
    let mut out: Vec<String> = Vec::new();
    let mut written = false;
    let mut in_table = false;
    for text_line in text.lines() {
        let trimmed = text_line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_table = true;
        } else if !in_table && !written && trimmed.starts_with("default_model") {
            out.push(line.clone());
            written = true;
            continue;
        }
        out.push(text_line.to_string());
    }
    if !written {
        out.insert(0, line);
    }
    fs::write(path, format!("{}\n", out.join("\n"))).with_context(|| format!("could not write {}", path.display()))
}

/// The first-launch guide: on the first interactive start, offers to add another command
/// beside the `killi` binary (a symlink such as `dw` -> `killi`). Pressing Enter, or typing
/// `killi`, keeps the default. Invalid or already-taken names are explained and asked again;
/// choosing one records the choice in `~/.killi/command-guide.json` so it is asked only once.
/// Nothing is overwritten: an existing command with that name is left alone.
pub fn command_guide_if_first() -> Result<()> {
    let path = killi_home().join("command-guide.json");
    if path.exists() || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    let invoked = std::env::args().next().and_then(|arg| PathBuf::from(arg).file_name().map(|name| name.to_string_lossy().into_owned()));
    if invoked.as_deref() != Some("killi") {
        return Ok(());
    }
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    if exe.file_name().is_none_or(|name| name != "killi") {
        return Ok(());
    }
    let chosen = choose_command(&exe)?;
    fs::create_dir_all(killi_home())?;
    fs::write(&path, format!("{}\n", serde_json::json!({ "command": chosen }))).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

fn choose_command(exe: &Path) -> Result<String> {
    eprintln!("Welcome to killi. You can start it as `killi`, or pick a shorter command such as `dw`.");
    loop {
        eprint!("Start command [killi]: ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer)? == 0 {
            return Ok("killi".to_string());
        }
        let name = answer.trim();
        if name.is_empty() || name == "killi" {
            return Ok("killi".to_string());
        }
        if !valid_command_name(name) {
            eprintln!("`{name}` can't be a command: use 1-32 letters, digits, `-` or `_`, starting with a letter, digit or `_`. Try another, or press Enter to keep `killi`.");
            continue;
        }
        match create_command_link(exe, name) {
            Ok(link) => {
                eprintln!("Created {} -> killi. Start with `{name}` or `killi`.", link.display());
                return Ok(name.to_string());
            }
            Err(reason) => eprintln!("{reason} Try another name, or press Enter to keep `killi`."),
        }
    }
}

fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().next().is_some_and(|first| first.is_ascii_alphanumeric() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Links `name`, beside `exe`, to the binary's file name. A link that already points at `exe`
/// counts as done; any other existing file or command is reported, never replaced.
fn create_command_link(exe: &Path, name: &str) -> std::result::Result<PathBuf, String> {
    let dir = exe.parent().ok_or_else(|| format!("{} has no folder", exe.display()))?;
    let target = exe.file_name().ok_or_else(|| format!("{} has no file name", exe.display()))?;
    let link = dir.join(name);
    match fs::symlink_metadata(&link) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                && let Ok(existing) = fs::read_link(&link)
            {
                let resolved = if existing.is_absolute() { existing } else { dir.join(existing) };
                if fs::canonicalize(&resolved).is_ok_and(|path| path == exe) {
                    return Ok(link);
                }
            }
            return Err(format!("{} already exists and was not changed.", link.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not check {}: {error}.", link.display())),
    }
    if let Some(existing) = command_on_path(name, dir) {
        return Err(format!("`{name}` already runs {}; it was not changed.", existing.display()));
    }
    std::os::unix::fs::symlink(target, &link).map_err(|error| format!("could not create {}: {error}.", link.display()))?;
    Ok(link)
}

fn command_on_path(name: &str, except: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).filter(|dir| dir != except).map(|dir| dir.join(name)).find(|candidate| {
            fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Asks a yes/no question on the terminal; Enter means yes.
fn confirm(question: &str) -> Result<bool> {
    Ok(matches!(read_line(&format!("{question} [Y/n] "))?.to_ascii_lowercase().as_str(), "" | "y" | "yes"))
}

/// Prints `prompt` and reads one line from the terminal, trimmed; end of input reads as no
/// answer.
fn read_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Ok(String::new());
    }
    Ok(answer.trim().to_string())
}

/// Reads one line with the terminal's echo off, so a pasted key stays out of the scrollback.
/// Ctrl+C cancels the paste: with signals off for the read it arrives as a byte, and reads as
/// no answer rather than ending killi with the echo still off.
fn read_secret(prompt: &str) -> Result<String> {
    let mut settings = unsafe { std::mem::zeroed::<libc::termios>() };
    // SAFETY: stdin is a terminal here and `settings` is initialized before it is read into.
    let terminal = unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut settings) } == 0;
    let saved = settings;
    if terminal {
        settings.c_lflag &= !(libc::ECHO | libc::ISIG);
        // SAFETY: the settings were just read from this terminal.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &settings) };
    }
    let answer = read_line(prompt);
    if terminal {
        // SAFETY: `saved` is this terminal's own settings, read before the change.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved) };
        eprintln!();
    }
    let answer = answer?;
    Ok(if answer.contains('\u{3}') { String::new() } else { answer })
}

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

fn import_pi_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
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

fn import_omp_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
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

fn import_opencode_from(config_path: &Path, auth_path: &Path, dst_dir: &Path, force: bool) -> Result<()> {
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

/// Reads a YAML config, as omp writes it, into the shape `read_json` returns. Only the
/// block style omp's own writer emits — mappings and sequences by indentation, plain,
/// quoted and single-line flow scalars — is understood; aliases, block scalars and other
/// spelled-out YAML are reported rather than guessed at, so an unimportable file fails the
/// import instead of importing wrong. A `.yaml` sibling stands in for a missing `.yml`.
fn read_yaml(path: &Path) -> Result<Option<serde_json::Value>> {
    let present = if path.exists() { path.to_path_buf() } else { path.with_extension("yaml") };
    let text = match fs::read_to_string(&present) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("could not read {}", present.display())),
    };
    let mut lines: Vec<(usize, usize, &str)> = Vec::new();
    for (at, raw) in text.lines().enumerate() {
        let line = strip_yaml_comment(raw.trim_end());
        if line.trim().is_empty() {
            continue;
        }
        if line.trim() == "---" {
            if lines.is_empty() {
                continue;
            }
            bail!("YAML in {} has more than one document (line {})", present.display(), at + 1);
        }
        let (Some(spaces), None) = (line.bytes().position(|b| b != b' '), line.find('\t')) else {
            bail!("YAML in {} indents with tabs (line {})", present.display(), at + 1);
        };
        lines.push((spaces, at + 1, line[spaces..].trim_end()));
    }
    if lines.is_empty() {
        return Ok(Some(serde_json::Value::Null));
    }
    let at = lines[0].1;
    let (value, used) = yaml_block(&lines, 0, lines[0].0).map_err(|reason| anyhow::anyhow!("{reason} in {}", present.display()))?;
    if used != lines.len() {
        bail!("could not read the YAML in {} past line {at}", present.display());
    }
    Ok(Some(value))
}

/// The part of a YAML line before any comment: a `#` starts one only between words, and a
/// quoted span keeps its own.
fn strip_yaml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote = 0u8;
    let mut i = 0;
    while i < bytes.len() {
        match (quote, bytes[i]) {
            (b'"', b'\\') => i += 1,
            (b'"', b'"') | (b'\'', b'\'') => quote = 0,
            (0, b'"' | b'\'') => quote = bytes[i],
            (0, b'#') if i == 0 || bytes[i - 1] == b' ' => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// One block node at `indent` from `lines[start..]`: its value and the index after it.
fn yaml_block(lines: &[(usize, usize, &str)], start: usize, indent: usize) -> std::result::Result<(serde_json::Value, usize), String> {
    if lines[start].2.starts_with("- ") || lines[start].2 == "-" {
        return yaml_sequence(lines, start, indent);
    }
    if yaml_key(lines[start].2).is_some() {
        return yaml_mapping(lines, start, indent);
    }
    Ok((yaml_scalar(lines[start].2)?, start + 1))
}

/// A block mapping whose keys sit at `indent`: each `key:` takes the value behind it, or
/// the deeper block under it.
fn yaml_mapping(lines: &[(usize, usize, &str)], start: usize, indent: usize) -> std::result::Result<(serde_json::Value, usize), String> {
    let mut map = serde_json::Map::new();
    let mut i = start;
    while i < lines.len() && lines[i].0 == indent {
        let (key, rest) = yaml_key(lines[i].2).ok_or_else(|| format!("expected a `key:` at line {}", lines[i].1))?;
        i += 1;
        map.insert(key, if rest.is_empty() { yaml_nested(lines, &mut i, indent)? } else { yaml_scalar(rest)? });
    }
    Ok((serde_json::Value::Object(map), i))
}

/// A `key:` with nothing behind it: the block under it, a sequence sitting at the key's own
/// indent (the one case YAML indents no deeper), or nothing.
fn yaml_nested(lines: &[(usize, usize, &str)], i: &mut usize, indent: usize) -> std::result::Result<serde_json::Value, String> {
    if *i >= lines.len() {
        return Ok(serde_json::Value::Null);
    }
    let (next, _, text) = lines[*i];
    if next > indent {
        let (value, used) = yaml_block(lines, *i, next)?;
        *i = used;
        Ok(value)
    } else if next == indent && (text.starts_with("- ") || text == "-") {
        let (value, used) = yaml_sequence(lines, *i, indent)?;
        *i = used;
        Ok(value)
    } else {
        Ok(serde_json::Value::Null)
    }
}

/// A block sequence whose dashes sit at `indent`. An item that opens a mapping — `- id: x`
/// with the rest aligned after the dash — stands as its own block, so its lines re-indent
/// to that column before the mapping reads them.
fn yaml_sequence(lines: &[(usize, usize, &str)], start: usize, indent: usize) -> std::result::Result<(serde_json::Value, usize), String> {
    let mut items = Vec::new();
    let mut i = start;
    while i < lines.len() && lines[i].0 == indent && (lines[i].2.starts_with("- ") || lines[i].2 == "-") {
        let (_, at, text) = lines[i];
        let inner = indent + 2;
        i += 1;
        let head = text.strip_prefix('-').unwrap_or_default().trim_start();
        if head.is_empty() {
            items.push(yaml_nested(lines, &mut i, indent)?);
        } else if yaml_key(head).is_some() {
            let mut block = vec![(inner, at, head)];
            while i < lines.len() && lines[i].0 >= inner {
                block.push(lines[i]);
                i += 1;
            }
            let (value, _) = yaml_mapping(&block, 0, inner)?;
            items.push(value);
        } else {
            items.push(yaml_scalar(head)?);
        }
    }
    Ok((serde_json::Value::Array(items), i))
}

/// A line's `key:` split, when it opens a mapping entry: the plain key (a `/` in a model id
/// stays, a `:` only ends the key before a space or the line's end) and what follows it.
fn yaml_key(line: &str) -> Option<(String, &str)> {
    if line.starts_with(['[', '{', '"', '\'']) {
        return None;
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            let key = &line[..i];
            if key.is_empty() || key.contains([' ', ',', '[', ']', '{', '}']) {
                return None;
            }
            return Some((key.to_string(), line[i + 1..].trim_start()));
        }
        i += 1;
    }
    None
}

/// A scalar as omp's writer spells it: a quoted string, a one-line flow collection, or a
/// plain value read as null, bool, number or text. An anchor's name drops — omp's
/// `!command` keys stay text, for the importer to decline — and an alias or block scalar is
/// declined, not guessed.
fn yaml_scalar(text: &str) -> std::result::Result<serde_json::Value, String> {
    let text = text.trim();
    let text = match text.strip_prefix('&') {
        Some(after) => after.split_once(' ').map_or("", |(_, value)| value).trim_start(),
        None => text,
    };
    match text.chars().next() {
        Some(c @ ('*' | '|' | '>')) => Err(format!("unsupported YAML scalar starting `{c}`")),
        Some('"') => serde_json::from_str::<serde_json::Value>(text).map_err(|_| format!("bad quoted string `{text}`")),
        Some('\'') if text.len() >= 2 && text.ends_with('\'') => Ok(text[1..text.len() - 1].replace("''", "'").into()),
        Some('[') if text.ends_with(']') => {
            let inner = text[1..text.len() - 1].trim();
            if inner.is_empty() {
                Ok(serde_json::Value::Array(Vec::new()))
            } else {
                inner.split(',').map(yaml_scalar).collect::<std::result::Result<Vec<_>, _>>().map(serde_json::Value::Array)
            }
        }
        Some('{') if text.ends_with('}') => {
            let inner = text[1..text.len() - 1].trim();
            let mut map = serde_json::Map::new();
            if !inner.is_empty() {
                for entry in inner.split(',') {
                    let (key, value) = entry.split_once(':').ok_or_else(|| format!("bad flow mapping `{text}`"))?;
                    map.insert(match yaml_scalar(key)? {
                        serde_json::Value::String(key) => key,
                        other => other.to_string(),
                    }, yaml_scalar(value)?);
                }
            }
            Ok(serde_json::Value::Object(map))
        }
        _ => Ok(match text {
            "null" | "Null" | "NULL" | "~" | "" => serde_json::Value::Null,
            "true" | "True" | "TRUE" => true.into(),
            "false" | "False" | "FALSE" => false.into(),
            _ if text.parse::<i64>().is_ok() => text.parse::<i64>().unwrap_or_default().into(),
            _ if text.contains(['.', 'e', 'E']) && !text.starts_with(['+', '.']) && text.parse::<f64>().is_ok() => {
                text.parse::<f64>().unwrap_or_default().into()
            }
            _ => text.to_string().into(),
        }),
    }
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

/// A token count as a TOML integer.
fn toml_int(value: u64) -> toml::Value {
    toml::Value::Integer(value as i64)
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

    #[test]
    fn a_missing_key_variable_names_the_variable_and_the_fix() {
        let config: super::Config = toml::from_str(
            "default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n",
        )
        .unwrap();
        let error = config.resolve(None).unwrap_err().to_string();
        assert!(error.contains("model `test/model`"), "{error}");
        assert!(error.contains("$KILLI_TEST_ABSENT_KEY"), "{error}");
        assert!(error.contains("[providers.test]"), "{error}");
        assert!(error.contains("~/.killi/config.toml"), "{error}");
        assert!(!error.contains("environment variable not found"), "{error}");
    }

    #[test]
    fn an_empty_key_variable_counts_as_missing() {
        // SAFETY: no other test reads or writes KILLI_TEST_EMPTY_KEY.
        unsafe { std::env::set_var("KILLI_TEST_EMPTY_KEY", "") };
        let config: super::Config = toml::from_str(
            "default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_EMPTY_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n",
        )
        .unwrap();
        let error = config.resolve(None).unwrap_err().to_string();
        assert!(error.contains("$KILLI_TEST_EMPTY_KEY"), "{error}");
    }

    #[test]
    fn missing_key_names_the_model_and_provider_that_cant_start() {
        let source = "[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[providers.inline]\ntype = \"openai\"\napi_key = \"sk-inline\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n\n[models.\"inline/model\"]\nprovider = \"inline\"\nmodel = \"model\"\n";
        let config: super::Config = toml::from_str(&format!("default_model = \"test/model\"\n{source}")).unwrap();
        let missing = config.missing_key(None).unwrap();
        assert_eq!(missing.alias, "test/model");
        assert_eq!(missing.provider, "test");
        assert!(config.missing_key(Some("inline/model")).is_none(), "an inline key is a key");
        assert!(config.missing_key(Some("unknown/model")).is_none(), "resolve reports those");

        let blank: super::Config = toml::from_str("default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key = \"\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();
        assert!(blank.missing_key(None).is_some(), "a blank key is no key");
    }

    #[test]
    fn storing_a_key_replaces_the_environment_line_and_keeps_the_rest() {
        let root = std::env::temp_dir().join(format!("killi-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        std::fs::write(&path, "# my config\ndefault_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();

        super::set_provider_key(&path, "test", "sk-pasted").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"), "{text}");
        assert!(!text.contains("api_key_env"), "{text}");
        let config: super::Config = toml::from_str(&text).unwrap();
        assert_eq!(config.providers["test"].api_key.as_deref(), Some("sk-pasted"));
        assert!(config.missing_key(None).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn choosing_a_model_rewrites_the_default_model_line() {
        let root = std::env::temp_dir().join(format!("killi-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        std::fs::write(&path, "default_model = \"test/first\"\n\n[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n").unwrap();
        super::set_default_model(&path, "test/second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "default_model = \"test/second\"\n\n[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n");

        std::fs::write(&path, "[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n").unwrap();
        super::set_default_model(&path, "test/first").unwrap();
        let config: super::Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.default_model.as_deref(), Some("test/first"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn importing_pi_carries_over_providers_models_and_the_default() {
        let root = std::env::temp_dir().join(format!("killi-import-pi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("pi");
        let dst = root.join("killi");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("models.json"), serde_json::json!({"providers": {
            "sph": {"baseUrl": "https://gw.example.com/v1", "api": "openai-completions", "apiKey": "sk-sph", "models": [
                {"id": "glm-5.3", "reasoning": true, "input": ["text", "image"], "contextWindow": 1000000, "maxTokens": 128000}
            ]},
            "minimax": {"apiKey": "sk-mm"},
            "nokey": {"baseUrl": "https://x/v1", "models": [{"id": "m"}]}
        }}).to_string()).unwrap();
        std::fs::write(src.join("models-store.json"), serde_json::json!({"minimax": {"models": [
            {"id": "MiniMax-M3", "api": "anthropic-messages", "baseUrl": "https://api.minimaxi.com/anthropic", "reasoning": true, "contextWindow": 196608, "maxTokens": 32768},
            {"id": "MiniMax-M3-Flash", "baseUrl": "https://api.minimaxi.com/anthropic"}
        ]}}).to_string()).unwrap();
        std::fs::write(src.join("settings.json"), serde_json::json!({"defaultProvider": "minimax", "defaultModel": "MiniMax-M3", "defaultThinkingLevel": "high"}).to_string()).unwrap();
        std::fs::write(src.join("mcp.json"), serde_json::json!({"mcpServers": {"x": {"command": "echo"}}}).to_string()).unwrap();

        super::import_pi_from(&src, &dst, false).unwrap();
        let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
        let config: super::Config = toml::from_str(&text).unwrap();
        assert_eq!(config.default_model.as_deref(), Some("minimax/MiniMax-M3"));
        assert_eq!(config.thinking.effort.as_deref(), Some("high"));
        assert_eq!(config.providers.len(), 2, "a provider without a key is skipped");
        assert_eq!(config.providers["sph"].api_key.as_deref(), Some("sk-sph"));
        assert_eq!(config.providers["sph"].base_url.as_deref(), Some("https://gw.example.com/v1"));
        assert_eq!(config.providers["minimax"].kind, "anthropic", "the catalog model's API names the protocol");
        assert_eq!(config.providers["minimax"].base_url.as_deref(), Some("https://api.minimaxi.com/anthropic"));
        let glm = &config.models["sph/glm-5.3"];
        assert_eq!((glm.max_context_size, glm.max_output_size), (Some(1_000_000), Some(128_000)));
        assert_eq!(glm.capabilities, ["thinking", "image_in"]);
        assert_eq!(config.models["minimax/MiniMax-M3-Flash"].protocol, None, "a model that names no API takes its provider's");
        assert_eq!(std::fs::read_to_string(dst.join("mcp.json")).unwrap(), std::fs::read_to_string(src.join("mcp.json")).unwrap());
        assert!(config.missing_key(None).is_none(), "the imported config starts as it is");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn importing_opencode_carries_over_providers_models_and_mcp() {
        let root = std::env::temp_dir().join(format!("killi-import-opencode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("opencode");
        let dst = root.join("killi");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("opencode.json"), serde_json::json!({
            "model": "minimax/MiniMax-M3",
            "provider": {
                "sph": {"npm": "@ai-sdk/openai-compatible", "options": {"baseURL": "https://gw.example.com/v1", "apiKey": "sk-sph"}, "models": {
                    "glm-5.3": {"name": "GLM 5.3", "reasoning": true, "attachment": true, "limit": {"context": 1000000, "output": 128000}}
                }},
                "volc": {"options": {"baseURL": "https://ark.example.com/v3"}, "models": {"doubao": {}}},
                "oauthonly": {"models": {"m": {}}}
            },
            "mcp": {
                "local": {"type": "local", "command": ["npx", "-y", "pkg"]},
                "remote": {"type": "remote", "url": "https://mcp.example.com"},
                "off": {"type": "local", "command": ["x"], "enabled": false}
            }
        }).to_string()).unwrap();
        std::fs::write(src.join("auth.json"), serde_json::json!({
            "volc": {"type": "api", "key": "sk-volc"},
            "oauthonly": {"type": "oauth", "refresh": "r"}
        }).to_string()).unwrap();

        super::import_opencode_from(&src.join("opencode.json"), &src.join("auth.json"), &dst, false).unwrap();
        let config: super::Config = toml::from_str(&std::fs::read_to_string(dst.join("config.toml")).unwrap()).unwrap();
        assert_eq!(config.default_model.as_deref(), Some("sph/glm-5.3"), "a default whose provider wasn't imported falls back");
        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.providers["sph"].api_key.as_deref(), Some("sk-sph"));
        assert_eq!(config.providers["volc"].api_key.as_deref(), Some("sk-volc"), "the key comes from auth.json");
        assert_eq!(config.providers["volc"].kind, "openai");
        let glm = &config.models["sph/glm-5.3"];
        assert_eq!((glm.max_context_size, glm.max_output_size), (Some(1_000_000), Some(128_000)));
        assert_eq!(glm.capabilities, ["thinking", "image_in"]);
        assert_eq!(config.models["volc/doubao"].model, "doubao");

        let mcp: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dst.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"]["local"]["command"], "npx");
        assert_eq!(mcp["mcpServers"]["local"]["args"], serde_json::json!(["-y", "pkg"]));
        assert_eq!(mcp["mcpServers"]["remote"]["url"], "https://mcp.example.com");
        assert!(mcp["mcpServers"].get("off").is_none(), "a disabled server is left out");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn importing_omp_carries_over_providers_models_and_the_default() {
        let root = std::env::temp_dir().join(format!("killi-import-omp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("omp");
        let dst = root.join("killi");
        std::fs::create_dir_all(&src).unwrap();
        // SAFETY: tests that read this variable don't run concurrently with one that changes it.
        unsafe { std::env::set_var("KILLI_TEST_OMP_KEY", "sk-env") };
        std::fs::write(src.join("models.yml"), r"# omp's models
providers:
  gateway:
    baseUrl: https://gw.example.com/v1 # the team gateway
    api: openai-completions
    apiKey: sk-gateway
    headers:
      X-Team: platform
    models:
      - id: glm-5.3
        name: GLM 5.3
        reasoning: true
        input: [text, image]
        cost:
          input: 0
          output: 0
        contextWindow: 1000000
        maxTokens: 128000
      - id: fast-chat
        contextWindow: 128000
  envkey:
    baseUrl: https://api.example.com/anthropic
    apiKey: KILLI_TEST_OMP_KEY
    models:
    - id: claude-x
      api: anthropic-messages
      reasoning: true
      maxTokens: 8192
  cmdkey:
    baseUrl: https://cmd.example.com/v1
    apiKey: !op read op://vault/item/key
    models:
      - id: m
  nokey:
    baseUrl: https://x/v1
    models:
      - id: m
").unwrap();
        std::fs::write(src.join("config.yml"), r"modelRoles:
  default: envkey/claude-x:high, gateway/glm-5.3
").unwrap();
        std::fs::write(src.join("mcp.json"), serde_json::json!({"mcpServers": {"x": {"command": "echo"}}}).to_string()).unwrap();

        super::import_omp_from(&src, &dst, false).unwrap();
        let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
        let config: super::Config = toml::from_str(&text).unwrap();
        assert_eq!(config.default_model.as_deref(), Some("envkey/claude-x"), "the `default` role, first of the list, with its `:effort` off the end");
        assert_eq!(config.thinking.effort.as_deref(), Some("high"));
        assert_eq!(config.providers.len(), 2, "a `!command` key and a missing key are skipped");
        assert_eq!(config.providers["gateway"].api_key.as_deref(), Some("sk-gateway"));
        assert_eq!(config.providers["gateway"].base_url.as_deref(), Some("https://gw.example.com/v1"), "the comment after the URL drops");
        assert_eq!(config.providers["gateway"].custom_headers.get("X-Team").map(String::as_str), Some("platform"));
        assert_eq!(config.providers["envkey"].api_key_env.as_deref(), Some("KILLI_TEST_OMP_KEY"), "a set variable's name reads as `api_key_env`");
        assert_eq!(config.providers["envkey"].kind, "anthropic", "the model's API names the protocol");
        let glm = &config.models["gateway/glm-5.3"];
        assert_eq!((glm.max_context_size, glm.max_output_size), (Some(1_000_000), Some(128_000)));
        assert_eq!(glm.capabilities, ["thinking", "image_in"]);
        assert_eq!(config.models["gateway/fast-chat"].max_context_size, Some(128_000));
        assert_eq!(std::fs::read_to_string(dst.join("mcp.json")).unwrap(), std::fs::read_to_string(src.join("mcp.json")).unwrap());
        assert!(config.missing_key(None).is_none(), "the imported config starts as it is");
        // SAFETY: as above.
        unsafe { std::env::remove_var("KILLI_TEST_OMP_KEY") };
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn omp_yaml_the_importer_cannot_read_is_reported() {
        let root = std::env::temp_dir().join(format!("killi-import-omp-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("models.yml"), "providers:\n  a:\n    apiKey: |\n      sk-1\n").unwrap();
        assert!(super::read_yaml(&root.join("models.yml")).unwrap_err().to_string().contains("unsupported YAML"));
        assert!(super::read_yaml(&root.join("missing.yml")).unwrap().is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn command_names_are_single_safe_words() {
        for name in ["dw", "ddd", "d", "my-cmd_2", "K9"] {
            assert!(super::valid_command_name(name), "{name}");
        }
        for name in ["", "-dw", "d w", "dw/killi", "dw;rm", "dw\n", &"d".repeat(33)] {
            assert!(!super::valid_command_name(name), "{name:?}");
        }
    }

    #[test]
    fn command_link_points_at_the_binary_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("killi-command-guide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("killi");
        std::fs::write(&exe, b"binary").unwrap();
        let exe = std::fs::canonicalize(exe).unwrap();

        let link = super::create_command_link(&exe, "dwtest").unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), std::path::Path::new("killi"));
        assert_eq!(super::create_command_link(&exe, "dwtest").unwrap(), link);

        std::fs::write(root.join("taken"), b"other").unwrap();
        assert!(super::create_command_link(&exe, "taken").unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read(root.join("taken")).unwrap(), b"other");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prompt_section_reads_back_as_the_built_in_guidance() {
        let text = format!("{}{}", super::TEMPLATE, super::prompt_section());
        let config: super::Config = toml::from_str(&text).unwrap();
        assert_eq!(config.prompt.system.as_deref(), Some(crate::prompt::DEFAULT));
        assert_eq!(config.guidance(), crate::prompt::DEFAULT);
        let blank: super::Config = toml::from_str("[prompt]\nsystem = \"  \"").unwrap();
        assert_eq!(blank.guidance(), crate::prompt::DEFAULT);
    }
}
