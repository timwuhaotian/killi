use super::*;
use std::fs;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

pub(super) const TEMPLATE: &str = r#"# killi's config. Models are named "<provider>/<model>"; pick one with -m or default_model.
default_model = "openai/gpt-5.5"

# [thinking]
# enabled = false              # turns reasoning off everywhere; each model's default_effort
#                              # (what /effort last picked) is used otherwise

[agent]
swarm = true                    # start in swarm mode: split work across subagents; false turns it off
peers = true                    # other killi sessions here can see this one, ask it and message it

[providers.openai]
type = "openai"                 # OpenAI chat completions, and compatible APIs
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"  # or: api_key = "..."

[providers.anthropic]
type = "anthropic"              # Anthropic Messages, and compatible APIs
api_key_env = "ANTHROPIC_API_KEY"

# To use your ChatGPT plan or an OpenCode Go subscription, run `killi login`: it signs in and
# adds that provider with its models here.

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
pub(super) fn prompt_section() -> String {
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
            None if config.chatgpt_model(Some(alias)).is_some() && !crate::login::chatgpt::signed_in() => "needs `killi login chatgpt`".to_string(),
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
