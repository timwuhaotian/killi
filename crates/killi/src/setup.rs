//! Everything a session is built from, decided once when killi starts: the model, the system
//! prompt and the tool list. Both the one-shot mode and the TUI start agents from here.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use serde_json::Value;

use crate::agent::{Agent, Controls, Settings, Sink};
use crate::config::Config;
use crate::llm::Client;
use crate::mcp::Mcp;
use crate::prompt::{self, SystemPrompt};
use crate::skills::Skills;
use crate::tools;

const DEFAULT_COMPACT_AT: u64 = 256_000;

pub struct Setup {
    config: Config,
    cwd: PathBuf,
    model: Option<String>,
    prompt: SystemPrompt,
    tools: Vec<Value>,
    settings: Settings,
    skills: Skills,
    /// Things worth telling the user at start, such as an MCP server that didn't start.
    notes: Vec<String>,
}

impl Setup {
    pub async fn new(config: Config, cwd: &Path, model: Option<&str>) -> Result<Self> {
        config.resolve(model)?;
        let skills = Skills::discover(cwd, &config.skills.dirs);
        let prompt = prompt::build(cwd, &skills);
        let mut tools = tools::definitions();
        if !skills.is_empty() {
            tools.push(crate::skills::definition());
        }
        let mut mcp = Mcp::load(cwd);
        let mut notes = Vec::new();
        if let Some(gateway) = mcp.definition() {
            tools.push(gateway);
            let mut eager = config.mcp.eager.clone();
            eager.extend(project_eager(cwd));
            eager.retain(|name| mcp.names().contains(&name.as_str()));
            eager.dedup();
            let (native, problems) = mcp.start_eager(&eager).await;
            tools.extend(native);
            notes.extend(problems);
        }
        let settings = Settings {
            compact_at: config.agent.compact_at.unwrap_or(DEFAULT_COMPACT_AT),
            plan_allow: config.agent.plan_allow.clone(),
            skills: skills.clone(),
            hooks: Rc::new(config.hooks.clone()),
            mcp: Rc::new(mcp),
            instruction_files: prompt.instruction_files.clone(),
        };
        Ok(Self { config, cwd: cwd.to_path_buf(), model: model.map(String::from), prompt, tools, settings, skills, notes })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn start(&self, controls: Controls, sink: Sink) -> Result<Agent> {
        let client = Client::new(self.config.resolve(self.model.as_deref())?)?;
        Agent::new(client, &self.cwd, &self.prompt.text, self.tools.clone(), self.settings.clone(), controls, sink)
    }

    /// Continues a logged session, on the model it used unless one was chosen with -m.
    pub fn resume(&self, log: &Path, controls: Controls, sink: Sink) -> Result<Agent> {
        let logged = crate::session::load(log)?.model;
        let alias = self.model.clone().or(logged.filter(|alias| self.config.models.contains_key(alias)));
        let client = Client::new(self.config.resolve(alias.as_deref())?)?;
        Agent::resume(client, &self.cwd, log, self.tools.clone(), self.settings.clone(), controls, sink)
    }

    /// The system prompt rebuilt from the files as they are now, for /reload.
    pub fn system_prompt(&self) -> String {
        prompt::build(&self.cwd, &Skills::discover(&self.cwd, &self.config.skills.dirs)).text
    }

    /// What the system prompt includes, in one line.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.prompt.instruction_files.is_empty() {
            let names: Vec<String> = self
                .prompt
                .instruction_files
                .iter()
                .map(|p| p.strip_prefix(&self.cwd).unwrap_or(p).display().to_string())
                .collect();
            parts.push(format!("instructions ~{} tokens from {}", self.prompt.instruction_tokens, names.join(", ")));
        }
        if !self.skills.is_empty() {
            parts.push(format!("{} skills", self.skills.len()));
        }
        if !self.settings.mcp.is_empty() {
            parts.push(format!("MCP: {}", self.settings.mcp.names().join(", ")));
        }
        parts.extend(self.notes.iter().cloned());
        parts.join(" · ")
    }

    pub fn instruction_tokens(&self) -> usize {
        self.prompt.instruction_tokens
    }

    pub fn instruction_files(&self) -> usize {
        self.prompt.instruction_files.len()
    }

    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    /// The prompt for `/name args` when `name` is a skill.
    pub fn skill_prompt(&self, name: &str, args: &str) -> Option<String> {
        self.skills.invocation(name, args)
    }
}

/// `[mcp] eager` from the project's `.killi/config.toml`.
fn project_eager(cwd: &Path) -> Vec<String> {
    let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
    let Ok(text) = std::fs::read_to_string(root.join(".killi/config.toml")) else { return Vec::new() };
    let Ok(table) = toml::from_str::<toml::Table>(&text) else { return Vec::new() };
    table
        .get("mcp")
        .and_then(|m| m.get("eager"))
        .and_then(toml::Value::as_array)
        .map(|names| names.iter().filter_map(|n| n.as_str().map(String::from)).collect())
        .unwrap_or_default()
}
