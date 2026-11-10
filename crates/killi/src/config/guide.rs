use super::*;
use std::fs;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

pub(super) const TEMPLATE: &str = r#"# killi's config. Models are named "<provider>/<model>". Add one from inside a session
# with /provider, or copy another agent's setup with `killi import`; /model picks one.
#
# To write one by hand, uncomment a block below and set default_model.
#
# default_model = "openai/gpt-5.5"
#
# [providers.openai]
# type = "openai"                 # OpenAI chat completions, and compatible APIs
# base_url = "https://api.openai.com/v1"
# api_key_env = "OPENAI_API_KEY"  # or: api_key = "..."
#
# [providers.anthropic]
# type = "anthropic"              # Anthropic Messages, and compatible APIs
# api_key_env = "ANTHROPIC_API_KEY"
#
# [models."openai/gpt-5.5"]
# provider = "openai"
# model = "gpt-5.5"
# protocol = "openai_responses"   # use the Responses API for this model
# max_context_size = 400000
# max_output_size = 128000
# capabilities = ["image_in", "thinking", "tool_use"]
#
# [models."anthropic/claude-sonnet-5-5"]
# provider = "anthropic"
# model = "claude-sonnet-5-5"
# max_context_size = 1000000
# max_output_size = 128000
# capabilities = ["image_in", "thinking", "tool_use"]
#
# To use your ChatGPT plan, or an OpenCode Go, Muse Code or Command Code subscription, run
# `killi login`: it signs in and adds that provider with its models here.

# [thinking]
# enabled = false              # turns reasoning off everywhere; each model's default_effort
#                              # (what /effort last picked) is used otherwise

[agent]
swarm = true                    # start in swarm mode: split work across subagents; false turns it off
swarm_parallel = 8              # subagents a swarm runs at once
peers = true                    # other killi sessions here can see this one, ask it and message it

# Keep a provider to the models you want. A provider without an entry keeps every model.
# [models_filter.zai]
# include = ["glm-5.3", "glm-5.3-flash"]   # aliases or model ids; a trailing * is a prefix
# exclude = ["*-preview"]
"#;

/// The OpenAI provider and model, added to a new config when its key is already in the
/// environment; without one the block stays a commented example in `TEMPLATE`.
const OPENAI: &str = r#"
[providers.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[models."openai/gpt-5.5"]
provider = "openai"
model = "gpt-5.5"
protocol = "openai_responses"
max_context_size = 400000
max_output_size = 128000
capabilities = ["image_in", "thinking", "tool_use"]
"#;

/// The Anthropic provider and model, added the same way as `OPENAI`.
const ANTHROPIC: &str = r#"
[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[models."anthropic/claude-sonnet-5-5"]
provider = "anthropic"
model = "claude-sonnet-5-5"
max_context_size = 1000000
max_output_size = 128000
capabilities = ["image_in", "thinking", "tool_use"]
"#;

/// The `[prompt]` table with the built-in guidance, as `init` and the importers write it.
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
        let table: toml::Table = toml::from_str(&text)
            .with_context(|| format!("invalid config in {}", path.display()))?;
        if table.contains_key("prompt") {
            bail!("{} already exists and has a [prompt] table", path.display());
        }
        let mut file = fs::OpenOptions::new().append(true).open(&path)?;
        if !text.is_empty() && !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        file.write_all(prompt_section().as_bytes())?;
        println!(
            "Added the system prompt to {} under [prompt]; edit it there.",
            path.display()
        );
        return Ok(());
    }
    write_template(&path)?;
    println!(
        "Wrote {}. Add a provider with /provider in a session, or edit it here.",
        path.display()
    );
    Ok(())
}

/// The config a new file starts with: the examples in `TEMPLATE`, plus an active provider
/// and model for each API key already in the environment, so a machine that is set up starts
/// configured. With no key, no provider or model is written at all.
fn starting_config() -> String {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let (openai, anthropic) = (set("OPENAI_API_KEY"), set("ANTHROPIC_API_KEY"));
    let mut text = String::new();
    if openai {
        text.push_str("default_model = \"openai/gpt-5.5\"\n");
    } else if anthropic {
        text.push_str("default_model = \"anthropic/claude-sonnet-5-5\"\n");
    }
    text.push_str(TEMPLATE);
    if openai {
        text.push_str(OPENAI);
    }
    if anthropic {
        text.push_str(ANTHROPIC);
    }
    text
}

/// Writes the starting config and its `[prompt]` table, making `~/.killi` owner-only.
fn write_template(path: &Path) -> Result<()> {
    let dir = path.parent().expect("the config path has a folder");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(starting_config().as_bytes())?;
    file.write_all(prompt_section().as_bytes())?;
    Ok(())
}

/// An interactive start without a config writes the starting config and goes on: the session
/// opens, and `/provider` adds a provider and its key from inside it. A provider whose key is
/// already in the environment is written and is the default.
pub fn start_if_missing() -> Result<()> {
    let path = killi_home().join("config.toml");
    if path.exists() || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    write_template(&path)?;
    let config = Config::load()?;
    let mut configured = config.resolve(None).is_ok();
    if !configured
        && let Some(alias) = config.first_keyed()
    {
        set_default_model(&path, alias)?;
        configured = true;
    }
    eprintln!(
        "✻ Wrote a starting config to {}.{}",
        path.display(),
        if configured {
            ""
        } else {
            " Add a provider from the session with /provider."
        }
    );
    Ok(())
}
