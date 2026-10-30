//! Everything a session is built from, decided when killi starts: the model, the system prompt
//! and the tool list. `/reload` re-reads the prompt and skills; the tools stay as they started.
//! Both the one-shot mode and the TUI start agents from here.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::agent::{Agent, Controls, Settings, Sink};
use crate::config::{Config, Thinking};
use crate::llm::Client;
use crate::mcp::Mcp;
use crate::prompt::{self, SystemPrompt};
use crate::session::{self, Choice};
use crate::skills::Skills;
use crate::tools;

const DEFAULT_SWARM_PARALLEL: usize = 8;

/// What a session starts with besides its model and folder, for the banner.
#[derive(Debug, Default)]
pub struct Overview {
    /// The prompt's guidance is `[prompt] system` from the config, not the built-in text.
    pub custom_prompt: bool,
    /// Instruction files (AGENTS.md), relative to the folder when they are in it.
    pub instructions: Vec<PathBuf>,
    pub instruction_tokens: usize,
    pub skills: usize,
    /// MCP servers that are on.
    pub mcp: Vec<String>,
    /// Open entries noted for later in this folder.
    pub later: usize,
    /// What went wrong starting up, such as an MCP server that didn't start.
    pub problems: Vec<String>,
}

pub struct Setup {
    config: Config,
    cwd: PathBuf,
    /// The model chosen with -m.
    model: Option<String>,
    /// The last /model pick, which a new session starts on without -m.
    last: Choice,
    prompt: SystemPrompt,
    tools: Vec<Value>,
    settings: Settings,
    skills: Skills,
    /// Things worth telling the user at start, such as an MCP server that didn't start.
    notes: Vec<String>,
}

impl Setup {
    pub async fn new(config: Config, cwd: &Path, model: Option<&str>) -> Result<Self> {
        let last = if model.is_none() { session::last_choice() } else { Choice::default() };
        config.resolve(model.or(known(&config, &last)))?;
        let skills = Skills::discover(cwd, &config.skills.dirs);
        let prompt = prompt::build(cwd, &skills, config.guidance());
        let mut tools = tools::definitions();
        if !skills.is_empty() {
            tools.push(crate::skills::definition());
        }
        let mut mcp = Mcp::load(cwd);
        let mut notes = Vec::new();
        if let Some(gateway) = mcp.definition() {
            tools.push(gateway);
            // A global eager name whose definition the project's `.mcp.json` replaced is a
            // project server, not a user one: it needs the trust check below, not a free start.
            let mut eager: Vec<String> = config.mcp.eager.iter().filter(|name| !mcp.is_project(name)).cloned().collect();
            let global_project: Vec<String> = config.mcp.eager.iter().filter(|name| mcp.is_project(name)).cloned().collect();
            eager.extend(trusted_project_eager(cwd, &mcp, &mut notes, &global_project));
            eager.retain(|name| mcp.names().contains(&name.as_str()));
            dedupe_eager(&mut eager);
            let (native, problems) = mcp.start_eager(&eager).await;
            tools.extend(native);
            notes.extend(problems);
        }
        let settings = Settings {
            compact_at: config.agent.compact_at,
            plan_allow: config.agent.plan_allow.clone(),
            swarm_parallel: config.agent.swarm_parallel.unwrap_or(DEFAULT_SWARM_PARALLEL).max(1),
            search: config.search.settings(),
            skills: skills.clone(),
            hooks: Rc::new(config.hooks.clone()),
            mcp: Rc::new(mcp),
            instruction_files: prompt.instruction_files.clone(),
        };
        Ok(Self { config, cwd: cwd.to_path_buf(), model: model.map(String::from), last, prompt, tools, settings, skills, notes })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A new session: on the -m model, else the last /model pick, else the default; its
    /// effort is the model's own `default_effort`, where `/effort` keeps the last pick.
    pub fn start(&self, controls: Controls, sink: Sink) -> Result<Agent> {
        self.start_on(self.client(&self.last)?, controls, sink)
    }

    /// A new session on `client`, as /new keeps the model and effort in use.
    pub fn start_on(&self, client: Client, controls: Controls, sink: Sink) -> Result<Agent> {
        let mut agent = Agent::new(client, &self.cwd, &self.prompt.text, self.tools.clone(), self.settings.clone(), controls, sink)?;
        if let Some(backup) = self.backup_client() {
            agent.set_backup(backup);
        }
        Ok(agent)
    }

    /// Continues a logged session, on the model and effort it last used unless -m chose one.
    /// The log parses once: the choice comes from the same load the agent replays.
    pub fn resume(&self, log: &Path, controls: Controls, sink: Sink) -> Result<Agent> {
        let report = session::load_reported(log)?;
        self.resume_loaded(report, log, controls, sink)
    }

    /// `resume` from the log's load, so callers that already show it don't parse it again:
    /// replay the conversation from `report.loaded` first, then move the report here.
    pub fn resume_loaded(&self, report: session::LoadReport, log: &Path, controls: Controls, sink: Sink) -> Result<Agent> {
        let client = self.client(&report.loaded.choice)?;
        Agent::resume_reported(client, self.backup_client(), &self.cwd, log, report, self.tools.clone(), self.settings.clone(), controls, sink)
    }

    /// The backup client the config's `backup_model` names, if it resolves: every session —
    /// new or resumed — starts on it. A backup the config no longer knows is dropped.
    fn backup_client(&self) -> Option<Client> {
        self.config.backup_model.as_deref().and_then(|alias| self.config.resolve(Some(alias)).and_then(Client::new).ok())
    }

    /// The -m model at its default effort, else `choice` when the config still has that model,
    /// at the effort the session log last recorded for it when the model offers it, else the
    /// default model.
    fn client(&self, choice: &Choice) -> Result<Client> {
        let chosen = known(&self.config, choice).filter(|_| self.model.is_none());
        let mut model = self.config.resolve(self.model.as_deref().or(chosen))?;
        if let Some(effort) = choice.effort.as_ref().filter(|e| chosen.is_some() && self.config.efforts(&model.alias).contains(e)) {
            model.thinking = Thinking::parse(effort);
        }
        Client::new(model)
    }

    /// Applies an `/effort` pick as the model's `default_effort`, so the resolves of this
    /// run — `/model` switches, later sessions — begin on it too, as the config file does.
    pub fn set_effort(&mut self, alias: &str, effort: &str) {
        if let Some(model) = self.config.models.get_mut(alias) {
            model.default_effort = Some(effort.to_string());
        }
    }

    /// Re-reads the config, skills and instruction files for /reload, so the system prompt,
    /// the skill tool, `/name` skill commands and later sessions all see them as they are now.
    /// Models, MCP servers and the tool list stay as they started.
    pub fn reload(&mut self) -> Result<()> {
        self.refresh(Config::load()?);
        Ok(())
    }

    fn refresh(&mut self, config: Config) {
        self.skills = Skills::discover(&self.cwd, &config.skills.dirs);
        // Without the skill tool the model can't load skills, so the prompt doesn't name them.
        let listed = if self.has_skill_tool() { self.skills.clone() } else { Skills::default() };
        self.prompt = prompt::build(&self.cwd, &listed, config.guidance());
        self.settings.skills = self.skills.clone();
        self.settings.instruction_files = self.prompt.instruction_files.clone();
        self.settings.search = config.search.settings();
        self.config = config;
    }

    /// Applies a `[search]` pick from /web-search-config: the settings stay the single
    /// source, so this session's agent and the sessions started later in this run of killi
    /// begin on it too.
    pub fn set_search(&mut self, search: crate::config::SearchConfig) {
        self.settings.search = search.settings();
        self.config.search = search;
    }

    /// Applies a `/backup-model` pick: sessions started later in this run of killi begin on
    /// it too, as every later run of killi does from the config file.
    pub fn set_backup_model(&mut self, alias: Option<&str>) {
        self.config.backup_model = alias.map(String::from);
    }

    /// Gives `agent` the reloaded system prompt and skills.
    pub fn reload_agent(&self, agent: &mut Agent) -> Result<()> {
        agent.reload_system(&self.prompt.text, self.settings.clone())
    }

    /// Whether skills found by /reload need a restart before the model can load them: the
    /// session started with none, so it has no skill tool.
    pub fn skills_need_restart(&self) -> bool {
        !self.skills.is_empty() && !self.has_skill_tool()
    }

    fn has_skill_tool(&self) -> bool {
        self.tools.iter().any(|tool| tool["function"]["name"] == "skill")
    }

    /// What the system prompt includes, in one line.
    /// What the session starts with besides its model and folder.
    pub fn overview(&self) -> Overview {
        Overview {
            custom_prompt: self.config.guidance() != prompt::DEFAULT,
            instructions: self.prompt.instruction_files.iter().map(|p| p.strip_prefix(&self.cwd).unwrap_or(p).to_path_buf()).collect(),
            instruction_tokens: self.prompt.instruction_tokens,
            skills: self.skills.len(),
            mcp: self.settings.mcp.names().iter().map(|name| name.to_string()).collect(),
            later: crate::later::list(&self.cwd).len(),
            problems: self.notes.clone(),
        }
    }

    /// The overview on one line, for headless runs.
    pub fn describe(&self) -> String {
        let overview = self.overview();
        let mut parts = Vec::new();
        if overview.custom_prompt {
            parts.push("custom prompt from [prompt]".to_string());
        }
        if !overview.instructions.is_empty() {
            let names: Vec<String> = overview.instructions.iter().map(|p| p.display().to_string()).collect();
            parts.push(format!("instructions ~{} tokens from {}", overview.instruction_tokens, names.join(", ")));
        }
        if overview.skills > 0 {
            parts.push(format!("{} skills", overview.skills));
        }
        if !overview.mcp.is_empty() {
            parts.push(format!("MCP: {}", overview.mcp.join(", ")));
        }
        if overview.later > 0 {
            parts.push(format!("{} noted for later · /later", overview.later));
        }
        parts.extend(overview.problems);
        parts.join(" · ")
    }

    pub fn instruction_tokens(&self) -> usize {
        self.prompt.instruction_tokens
    }

    pub fn instruction_files(&self) -> usize {
        self.prompt.instruction_files.len()
    }

    pub fn mcp(&self) -> &Mcp {
        &self.settings.mcp
    }

    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    /// The prompt for `/name args` when `name` is a skill.
    pub fn skill_prompt(&self, name: &str, args: &str) -> Option<String> {
        self.skills.invocation(name, args)
    }
}

/// The model `choice` names, when the config still has it.
fn known<'a>(config: &Config, choice: &'a Choice) -> Option<&'a str> {
    choice.model.as_deref().filter(|alias| config.models.contains_key(*alias))
}

/// The project killi runs in: the nearest folder with `.git`, else the working directory.
fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd).to_path_buf()
}

/// `[mcp] eager` from the project's `.killi/config.toml`.
fn project_eager(cwd: &Path) -> Vec<String> {
    let root = project_root(cwd);
    let Ok(text) = std::fs::read_to_string(root.join(".killi/config.toml")) else { return Vec::new() };
    let Ok(table) = toml::from_str::<toml::Table>(&text) else { return Vec::new() };
    table
        .get("mcp")
        .and_then(|m| m.get("eager"))
        .and_then(toml::Value::as_array)
        .map(|names| names.iter().filter_map(|n| n.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// The eager list without repeats, keeping the first of each: global eager plus project
/// eager can name the same server twice, and `Vec::dedup` only removes adjacent repeats.
fn dedupe_eager(eager: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    eager.retain(|name| seen.insert(name.clone()));
}

/// Project eager servers killi may start now: the trust store already approves them, or the
/// user does below. Anything else stays off for this session, with a note saying so, so a
/// "no" leaves killi fully usable without those servers.
fn trusted_project_eager(cwd: &Path, mcp: &Mcp, notes: &mut Vec<String>, global_project: &[String]) -> Vec<String> {
    let root = project_root(cwd);
    let mut wanted = project_eager(cwd);
    wanted.extend(global_project.iter().cloned());
    dedupe_eager(&mut wanted);
    // (name, what the prompt shows, what the fingerprint hashes)
    let entries: Vec<(String, String, String)> = wanted
        .into_iter()
        .filter(|name| mcp.names().contains(&name.as_str()))
        .filter_map(|name| Some((name.clone(), mcp.command_line(&name)?, mcp.trust_input(&name)?)))
        .collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let path_key = root.display().to_string();
    let fingerprint = trust_fingerprint(&entries.iter().map(|(name, _, input)| (name.clone(), input.clone())).collect::<Vec<_>>());
    if load_trusted(&trust_store_path()).get(&path_key).is_some_and(|known| *known == fingerprint) {
        return entries.into_iter().map(|(name, _, _)| name).collect();
    }
    if interactive() {
        // Project-controlled text is shown with escapes and controls stripped, so it
        // can't overwrite the very command the user is asked to inspect.
        eprintln!("The project at {} asks killi to run at startup:", shown(&path_key));
        for (name, line, _) in &entries {
            eprintln!("  {}: {}", shown(name), shown(line));
        }
        if crate::config::confirm("Trust and run these?").unwrap_or(false) {
            if let Err(error) = save_trusted(&trust_store_path(), &path_key, &fingerprint) {
                notes.push(format!("Trusted project servers, but could not write {}: {error:#}", trust_store_path().display()));
            }
            return entries.into_iter().map(|(name, _, _)| name).collect();
        }
    }
    notes.push(format!("Skipped project servers (untrusted): {}.", entries.iter().map(|(name, _, _)| name.as_str()).collect::<Vec<_>>().join(", ")));
    Vec::new()
}

/// Text from a project file made safe to print on the terminal.
fn shown(text: &str) -> String {
    crate::tui::text::sanitize(text).into_owned()
}

/// Whether killi can ask the user now; the first-run guides ask the same way.
fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Where per-repo startup approvals live.
fn trust_store_path() -> PathBuf {
    crate::config::killi_home().join("trusted.json")
}

/// The fingerprint of the command lines a project would run: any change re-asks.
fn trust_fingerprint(entries: &[(String, String)]) -> String {
    // FNV-1a, stable across toolchains, so an approval outlives an update.
    let mut hash = 0xcbf29ce484222325u64;
    for (name, line) in entries {
        for byte in name.bytes().chain([0]).chain(line.bytes()).chain([0]) {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

/// The approvals in `path`: none when it is missing or unreadable.
fn load_trusted(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// Remembers `fingerprint` for `root`, so this project's commands start without asking.
fn save_trusted(path: &Path, root: &str, fingerprint: &str) -> Result<()> {
    let mut store = load_trusted(path);
    store.insert(root.to_string(), fingerprint.to_string());
    crate::fs_atomic::write(path, serde_json::to_string(&store)?.as_bytes(), None)
        .with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests;
