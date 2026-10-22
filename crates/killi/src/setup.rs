//! Everything a session is built from, decided when killi starts: the model, the system prompt
//! and the tool list. `/reload` re-reads the prompt and skills; the tools stay as they started.
//! Both the one-shot mode and the TUI start agents from here.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use serde_json::Value;

use crate::agent::{Agent, Controls, Settings, Sink};
use crate::config::{Config, Thinking};
use crate::llm::Client;
use crate::mcp::Mcp;
use crate::prompt::{self, SystemPrompt};
use crate::session::{self, Choice};
use crate::skills::Skills;
use crate::tools;

const DEFAULT_COMPACT_AT: u64 = 256_000;
const DEFAULT_SWARM_PARALLEL: usize = 8;

pub struct Setup {
    config: Config,
    cwd: PathBuf,
    /// The model chosen with -m.
    model: Option<String>,
    /// The last pick from /model or /effort, which a new session starts on without -m.
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
            swarm_parallel: config.agent.swarm_parallel.unwrap_or(DEFAULT_SWARM_PARALLEL).max(1),
            search: crate::tools::SearchSettings {
                provider: config.search.provider.clone().unwrap_or_default(),
                api_key: config
                    .search
                    .api_key
                    .clone()
                    .or_else(|| config.search.api_key_env.as_deref().and_then(|name| std::env::var(name).ok()))
                    .filter(|key| !key.is_empty()),
            },
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

    /// A new session: on the -m model, else the last /model or /effort pick, else the default.
    pub fn start(&self, controls: Controls, sink: Sink) -> Result<Agent> {
        self.start_on(self.client(&self.last)?, controls, sink)
    }

    /// A new session on `client`, as /new keeps the model and effort in use.
    pub fn start_on(&self, client: Client, controls: Controls, sink: Sink) -> Result<Agent> {
        Agent::new(client, &self.cwd, &self.prompt.text, self.tools.clone(), self.settings.clone(), controls, sink)
    }

    /// Continues a logged session, on the model and effort it last used unless -m chose one.
    pub fn resume(&self, log: &Path, controls: Controls, sink: Sink) -> Result<Agent> {
        let client = self.client(&session::load(log)?.choice)?;
        Agent::resume(client, &self.cwd, log, self.tools.clone(), self.settings.clone(), controls, sink)
    }

    /// The -m model at its default effort, else `choice` when the config still has that model,
    /// at its effort when the model offers it, else the default model.
    fn client(&self, choice: &Choice) -> Result<Client> {
        let chosen = known(&self.config, choice).filter(|_| self.model.is_none());
        let mut model = self.config.resolve(self.model.as_deref().or(chosen))?;
        if let Some(effort) = choice.effort.as_ref().filter(|e| chosen.is_some() && self.config.efforts(&model.alias).contains(e)) {
            model.thinking = Thinking::parse(effort);
        }
        Client::new(model)
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
        self.config = config;
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
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.config.guidance() != prompt::DEFAULT {
            parts.push("custom prompt from [prompt]".to_string());
        }
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
        let later = crate::later::list(&self.cwd).len();
        if later > 0 {
            parts.push(format!("{later} noted for later · /later"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str) {
        let dir = root.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: Does {name}\n---\nBody of {name}.")).unwrap();
    }

    fn setup(root: &Path, tools: Vec<Value>) -> Setup {
        let config = fresh_config();
        let skills = Skills::discover(root, &config.skills.dirs);
        let prompt = prompt::build(root, &skills, config.guidance());
        let settings = Settings { skills: skills.clone(), instruction_files: prompt.instruction_files.clone(), ..Settings::default() };
        Setup { config, cwd: root.to_path_buf(), model: None, last: Choice::default(), prompt, tools, settings, skills, notes: Vec::new() }
    }

    fn fresh_config() -> Config {
        let mut config = Config::default();
        config.skills.dirs = vec!["/nonexistent".to_string()];
        config
    }

    #[test]
    fn reload_picks_up_skills_and_instructions_added_mid_session() {
        let root = std::env::temp_dir().join(format!("killi-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_skill(&root, "first");
        let mut setup = setup(&root, vec![crate::skills::definition()]);

        write_skill(&root, "second");
        std::fs::write(root.join("AGENTS.md"), "Always say hello.").unwrap();
        setup.refresh(fresh_config());

        assert!(setup.prompt.text.contains("first, second"), "{}", setup.prompt.text);
        assert!(setup.prompt.text.contains("Always say hello."));
        let under_root = |f: &&PathBuf| f.components().any(|c| Some(c.as_os_str()) == root.file_name());
        assert_eq!(setup.settings.instruction_files.iter().filter(under_root).count(), 1, "a global ~/.agents/AGENTS.md may add one");
        assert!(setup.settings.skills.run(&serde_json::json!({"name": "second"})).text.contains("Body of second."));
        assert!(setup.skill_prompt("second", "").is_some());
        assert!(!setup.skills_need_restart());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reload_without_the_skill_tool_keeps_skills_out_of_the_prompt() {
        let root = std::env::temp_dir().join(format!("killi-setup-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let mut setup = setup(&root, Vec::new());

        write_skill(&root, "late");
        setup.refresh(fresh_config());

        assert!(!setup.prompt.text.contains("# Skills"));
        assert!(setup.skill_prompt("late", "").is_some());
        assert!(setup.skills_need_restart());
        std::fs::remove_dir_all(root).unwrap();
    }
}
