//! Skills: folders holding a SKILL.md. Only their names go in the system prompt, about 4
//! tokens each; the `skill` tool loads one by name, or searches the descriptions locally
//! without spending model tokens. One skill is built in: `update-config`, which describes
//! this version's configuration files and keys.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Value, json};

use crate::config::home;
use crate::tools::Output;

/// The built-in skill for killi's own configuration; a test keeps it naming every key.
const UPDATE_CONFIG: &str = include_str!("skills/update-config.md");

#[derive(Clone, Default)]
pub struct Skills {
    list: Rc<Vec<Skill>>,
}

#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// The text of a built-in skill, which has no file.
    builtin: Option<&'static str>,
}

impl Skills {
    /// The project's `.agents/skills` and `.claude/skills` first, then `dirs`, which default
    /// to `~/.agents/skills`, then the built-in skills. The first skill with a given name wins.
    pub fn discover(cwd: &Path, dirs: &[String]) -> Self {
        let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
        let mut roots = vec![root.join(".agents/skills"), root.join(".claude/skills")];
        if dirs.is_empty() {
            roots.push(home().join(".agents/skills"));
        } else {
            roots.extend(dirs.iter().map(|d| match d.strip_prefix("~/") {
                Some(rest) => home().join(rest),
                None => PathBuf::from(d),
            }));
        }
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for dir in roots {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            let mut found: Vec<Skill> = entries.flatten().filter_map(|e| parse(&e.path().join("SKILL.md"))).collect();
            found.sort_by(|a, b| a.name.cmp(&b.name));
            for skill in found {
                if seen.insert(skill.name.clone()) {
                    list.push(skill);
                }
            }
        }
        if let Some(skill) = parse_text(UPDATE_CONFIG, Path::new("")).filter(|s| seen.insert(s.name.clone())) {
            list.push(Skill { builtin: Some(UPDATE_CONFIG), ..skill });
        }
        Self { list: Rc::new(list) }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.list.iter()
    }

    /// The system prompt's skills section.
    pub fn prompt_section(&self) -> String {
        if self.list.is_empty() {
            return String::new();
        }
        let names: Vec<&str> = self.list.iter().map(|s| s.name.as_str()).collect();
        format!(
            "\n# Skills\nSkills are instructions for specific tasks. When one may apply, load it with the skill tool before starting; if unsure which, search with a query. Available: {}.\n",
            names.join(", ")
        )
    }

    fn find(&self, name: &str) -> Option<&Skill> {
        self.list.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    fn content(skill: &Skill) -> Option<String> {
        if let Some(text) = skill.builtin {
            return Some(format!("<skill name=\"{}\">\n{}\n</skill>", skill.name, text.trim()));
        }
        let text = fs::read_to_string(&skill.path).ok()?;
        let folder = skill.path.parent().unwrap_or(Path::new("."));
        Some(format!("<skill name=\"{}\" folder=\"{}\">\n{}\n</skill>", skill.name, folder.display(), text.trim()))
    }

    /// The `skill` tool: load by `name`, or search with `query`.
    pub fn run(&self, args: &Value) -> Output {
        if let Some(name) = args["name"].as_str().filter(|n| !n.is_empty()) {
            return match self.find(name).and_then(Skills::content) {
                Some(text) => Output::ok(text).brief("loaded"),
                None => {
                    let close: Vec<String> = self.search(name).into_iter().map(|s| s.name.clone()).collect();
                    Output::error(format!("No skill named `{name}`. Closest: {}.", close.join(", ")))
                }
            };
        }
        let Some(query) = args["query"].as_str().filter(|q| !q.is_empty()) else {
            return Output::error("skill needs `name` or `query`.");
        };
        let found = self.search(query);
        if found.is_empty() {
            return Output::ok(format!("No skill matches “{query}”."));
        }
        let lines: Vec<String> = found.iter().map(|s| format!("- {}: {}", s.name, crate::llm::truncate(&s.description, 300))).collect();
        Output::ok(lines.join("\n")).brief(format!("{} matches", found.len()))
    }

    /// The five skills whose name and description share the most words with `query`.
    fn search(&self, query: &str) -> Vec<&Skill> {
        let words: Vec<String> = tokens(query);
        let mut scored: Vec<(usize, &Skill)> = self
            .list
            .iter()
            .map(|skill| {
                let name = tokens(&skill.name);
                let text = tokens(&skill.description);
                let score = words
                    .iter()
                    .map(|w| 3 * name.iter().filter(|n| n.starts_with(w.as_str())).count() + text.iter().filter(|t| t.starts_with(w.as_str())).count())
                    .sum();
                (score, skill)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        scored.into_iter().take(5).map(|(_, s)| s).collect()
    }

    /// The prompt for `/name args`, when `name` is a skill.
    pub fn invocation(&self, name: &str, args: &str) -> Option<String> {
        let skill = self.find(name)?;
        let content = Skills::content(skill)?;
        let request = if args.trim().is_empty() { "Follow the skill." } else { args.trim() };
        Some(format!("Use the {} skill for this.\n\n{content}\n\n{request}", skill.name))
    }
}

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1).map(String::from).collect()
}

/// Reads `name` and `description` from SKILL.md's front matter.
fn parse(path: &Path) -> Option<Skill> {
    parse_text(&fs::read_to_string(path).ok()?, path)
}

/// Reads `name` and `description` from a SKILL.md's front matter; the folder's name stands in
/// for a missing `name`.
fn parse_text(text: &str, path: &Path) -> Option<Skill> {
    let front = text.strip_prefix("---")?.split("\n---").next()?;
    let mut name = None;
    let mut description = String::new();
    let mut lines = front.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(unquote(value));
        } else if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim();
            if matches!(value, "|" | ">" | "|-" | ">-" | "") {
                while let Some(next) = lines.peek().filter(|l| l.starts_with(' ') || l.starts_with('\t')) {
                    description.push_str(next.trim());
                    description.push(' ');
                    lines.next();
                }
            } else {
                description = unquote(value);
            }
        }
    }
    let name = name.filter(|n| !n.is_empty()).or_else(|| path.parent()?.file_name()?.to_str().map(String::from))?;
    Some(Skill { name, description: description.trim().to_string(), path: path.to_path_buf(), builtin: None })
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(value).to_string()
}

pub fn definition() -> Value {
    json!({"type": "function", "function": {
        "name": "skill",
        "description": "Load a skill's instructions by name, or search skills by what they do.",
        "parameters": {"type": "object", "properties": {
            "name": {"type": "string"},
            "query": {"type": "string", "description": "Words describing the task"}
        }}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_parses_and_searches() {
        let root = std::env::temp_dir().join(format!("killi-skills-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        for (dir, front) in [
            ("ios-shots", "name: ios-shots\ndescription: Capture App Store screenshots on iOS simulators"),
            ("pdf", "name: pdf\ndescription: >-\n  Read and fill PDF forms\n  and merge files"),
        ] {
            fs::create_dir_all(root.join(".agents/skills").join(dir)).unwrap();
            fs::write(root.join(".agents/skills").join(dir).join("SKILL.md"), format!("---\n{front}\n---\nBody of {dir}.")).unwrap();
        }
        let skills = Skills::discover(&root, &["/nonexistent".to_string()]);
        assert_eq!(skills.len(), 3);
        assert!(skills.prompt_section().contains("Available: ios-shots, pdf, update-config."));
        assert_eq!(skills.find("pdf").unwrap().description, "Read and fill PDF forms and merge files");
        let found = skills.run(&json!({"query": "screenshots simulator"}));
        assert!(found.text.starts_with("- ios-shots:"), "{}", found.text);
        assert!(skills.run(&json!({"name": "pdf"})).text.contains("Body of pdf."));
        assert!(skills.invocation("pdf", "merge a.pdf b.pdf").unwrap().ends_with("merge a.pdf b.pdf"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_config_is_built_in_and_a_folder_skill_of_that_name_wins() {
        let root = std::env::temp_dir().join(format!("killi-builtin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let skills = Skills::discover(&root, &["/nonexistent".to_string()]);
        assert_eq!(skills.len(), 1);
        let loaded = skills.run(&json!({"name": "update-config"}));
        assert!(loaded.text.starts_with("<skill name=\"update-config\">\n---\nname: update-config"), "{}", loaded.text);
        assert!(loaded.text.contains("~/.killi/config.toml"));
        assert!(skills.invocation("update-config", "where is my config?").is_some());

        let dir = root.join(".agents/skills/update-config");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: update-config\ndescription: Mine\n---\nMy own.").unwrap();
        let skills = Skills::discover(&root, &["/nonexistent".to_string()]);
        assert_eq!(skills.len(), 1);
        assert!(skills.run(&json!({"name": "update-config"})).text.contains("My own."));
        fs::remove_dir_all(root).unwrap();
    }

    /// Adding a config key means describing it in the built-in skill, so it keeps up with
    /// the code: every key the config, hook and MCP server structs read must appear in it.
    #[test]
    fn update_config_names_every_config_key() {
        let words: HashSet<&str> = UPDATE_CONFIG.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
        let sources = [include_str!("config.rs"), include_str!("hooks.rs"), include_str!("mcp.rs")];
        let keys: Vec<String> = sources.iter().flat_map(|source| keys_read_by(source)).collect();
        for expected in ["default_model", "compact_at", "type", "matcher", "mcpServers", "toolTimeoutMs"] {
            assert!(keys.iter().any(|k| k == expected), "the key scanner missed `{expected}` in {keys:?}");
        }
        let missing: Vec<&String> = keys.iter().filter(|key| !words.contains(key.as_str())).collect();
        assert!(missing.is_empty(), "skills/update-config.md doesn't mention: {missing:?}");
    }

    /// The keys the `Deserialize` structs in `source` read, named as serde reads them.
    fn keys_read_by(source: &str) -> Vec<String> {
        let source = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut keys = Vec::new();
        let (mut deserialize, mut camel, mut inside, mut skip, mut rename) = (false, false, false, false, None::<String>);
        for line in source.lines().map(str::trim) {
            if !inside {
                if line.starts_with("#[derive(") {
                    deserialize = line.contains("Deserialize");
                } else if line.starts_with("#[serde(") {
                    camel = line.contains("camelCase");
                } else if line.contains("struct ") {
                    inside = deserialize && line.ends_with('{');
                    deserialize = false;
                }
                continue;
            }
            if line == "}" {
                (inside, camel) = (false, false);
            } else if let Some(attr) = line.strip_prefix("#[serde(") {
                skip |= attr.starts_with("skip");
                rename = attr.strip_prefix("rename = \"").and_then(|r| r.split('"').next()).map(String::from).or(rename);
            } else if let Some((field, _)) = line.strip_prefix("pub ").unwrap_or(line).split_once(':') {
                let field = field.trim();
                if field.chars().all(|c| c.is_alphanumeric() || c == '_') && !std::mem::take(&mut skip) {
                    keys.push(rename.take().unwrap_or_else(|| if camel { camel_case(field) } else { field.to_string() }));
                }
                rename = None;
            }
        }
        keys
    }

    fn camel_case(snake: &str) -> String {
        let mut out = String::new();
        let mut upper = false;
        for c in snake.chars() {
            match c {
                '_' => upper = true,
                c if upper => {
                    out.extend(c.to_uppercase());
                    upper = false;
                }
                c => out.push(c),
            }
        }
        out
    }
}
