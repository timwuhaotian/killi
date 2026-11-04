//! The sub-commands a skill declares for itself: playbooks in its own folder, like
//! impeccable's `polish`, that its SKILL.md routes to by name. A skill declares them in a
//! Commands table (a row per command, linking its playbook) and, for harnesses that
//! register them as skills, in a `command-metadata.json` beside the SKILL.md or under
//! `scripts/`. Only declared commands count: a `reference/` folder alone is material the
//! skill reads as it works, not names for the prompt, the slash menu or the `skill` tool.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::{Parent, Skill};

/// The skills for the sub-commands `parent` declares, in the order its table declares them,
/// then any further commands only its metadata names. A command whose playbook file isn't
/// in the parent's folder declares nothing.
pub(super) fn declared(parent: &Skill) -> Vec<Skill> {
    let Some(folder) = parent.path.parent() else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(&parent.path) else {
        return Vec::new();
    };
    let mut found: Vec<Skill> = Vec::new();
    let push = |found: &mut Vec<Skill>, name: String, description: String, path: PathBuf| {
        if valid(&name)
            && !name.eq_ignore_ascii_case(&parent.name)
            && path.is_file()
            && !found.iter().any(|s| s.name.eq_ignore_ascii_case(&name))
        {
            found.push(Skill {
                name,
                description,
                path,
                builtin: None,
                parent: Some(Parent {
                    name: parent.name.clone(),
                    path: parent.path.clone(),
                }),
            });
        }
    };
    for (name, description, target) in table(&text) {
        push(&mut found, name, description, folder.join(target));
    }
    for (name, description) in metadata(folder) {
        match found
            .iter_mut()
            .find(|s| s.name.eq_ignore_ascii_case(&name))
        {
            // The metadata's description is written for finding the command by what it
            // does, so it replaces the table's one line.
            Some(skill) if !description.is_empty() => skill.description = description,
            Some(_) => {}
            None => {
                let path = ["reference", "references"]
                    .iter()
                    .map(|dir| folder.join(dir).join(format!("{name}.md")))
                    .find(|p| p.is_file())
                    .unwrap_or_else(|| folder.join("reference").join(format!("{name}.md")));
                push(&mut found, name, description, path);
            }
        }
    }
    found
}

/// A command name as the slash menu and the `skill` tool take it: one word, not a path.
fn valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The `(name, description, playbook)` rows of SKILL.md's tables: a row whose first cell is
/// a backticked command (`| \`polish [target]\` | …`) and which links a `.md` file in the
/// folder. The description is the cell before the one holding the link, so a category
/// column between the command and it is skipped either way.
fn table(text: &str) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let Some((name, target)) = cells.first().and_then(|first| {
            let inside = first.strip_prefix('`')?.split('`').next()?;
            let name = inside
                .split_whitespace()
                .next()?
                .trim_start_matches(['$', '/']);
            Some((name.to_string(), link(line)?))
        }) else {
            continue;
        };
        let description = cells
            .iter()
            .rposition(|cell| cell.contains("]("))
            .and_then(|at| at.checked_sub(1).map(|at| cells[at]))
            .unwrap_or_default()
            .replace('`', "");
        rows.push((name, description, target));
    }
    rows
}

/// The first relative `.md` link in `line`, without its `#anchor`.
fn link(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some((_, tail)) = rest.split_once("](") {
        let target = tail
            .split([')', ' '])
            .next()
            .unwrap_or_default()
            .trim_matches(['<', '>']);
        let target = target.split('#').next().unwrap_or_default();
        if target.ends_with(".md") && !target.contains("://") {
            return Some(target.to_string());
        }
        rest = tail;
    }
    None
}

/// The `(name, description)` pairs of the folder's `command-metadata.json`, when it has
/// one beside the SKILL.md or under `scripts/`.
fn metadata(folder: &std::path::Path) -> Vec<(String, String)> {
    let text = [
        folder.join("command-metadata.json"),
        folder.join("scripts/command-metadata.json"),
    ]
    .iter()
    .find_map(|path| fs::read_to_string(path).ok());
    let Some(text) = text else {
        return Vec::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    map.iter()
        .map(|(name, info)| {
            (
                name.clone(),
                info["description"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}
