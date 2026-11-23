//! The user's memory: facts and preferences that hold in every project, in
//! `~/.killi/memory.md`. One entry per line; the whole list is sent in every session's system
//! prompt, so it stays small, and an add or edit past a limit is refused until something is
//! forgotten.

use std::fs;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::killi_home;

/// The most entries a memory holds: every session sends it with its first request.
pub const MAX_ITEMS: usize = 20;
/// The most bytes a memory holds, for the same reason.
pub const MAX_BYTES: usize = 4096;
/// The longest one entry may be, so a memory stays a fact rather than a document. A Chinese
/// fact of a few dozen characters fits comfortably.
pub const MAX_CHARS: usize = 240;

/// Where the memory lives: one file for the user, shared by every project.
pub fn path() -> PathBuf {
    killi_home().join("memory.md")
}

/// The saved entries, oldest first.
pub fn list() -> Vec<String> {
    list_in(&path())
}

/// What a session's prompt carries: the entries that fit the limits, and how many the file
/// holds, so a longer hand-edited file can be reported instead of sent whole.
pub struct Loaded {
    pub items: Vec<String>,
    pub total: usize,
}

pub fn for_prompt() -> Loaded {
    for_prompt_in(&path())
}

/// Saves `fact` with its whitespace made one space per run.
pub fn add(fact: &str) -> Result<Saved> {
    add_in(&path(), fact)
}

/// Removes entry `number` (counted from 1) and gives its text back with what is left.
pub fn forget(number: usize) -> Result<(String, usize)> {
    forget_in(&path(), number)
}

/// Where an added fact sits, or that the same fact was already saved.
#[derive(Debug, PartialEq)]
pub enum Saved {
    Added(usize),
    Already(usize),
}

/// What an edit did, and the text it touched, so the result can show the change.
#[derive(Debug, PartialEq)]
pub enum Edited {
    Replaced { at: usize, old: String, new: String },
    Removed { at: usize, old: String },
}

/// The system prompt's memory section, empty when nothing is saved.
pub fn section(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let list: String = items.iter().map(|item| format!("- {item}\n")).collect();
    format!(
        "\n# Memory\nWhat the user asked you to keep for every project. Apply it unless this conversation says otherwise; a saved note is not permission, the user's own words win, and an instruction file written for the work at hand wins over a general note.\n{list}"
    )
}

/// The entries numbered as `/memory forget N` and the `memory` tool count them, from 1.
pub fn numbered(items: &[String]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| format!("{}. {item}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a call changes the memory rather than reading it; plan mode and subagents use it.
pub fn writes(args: &Value) -> bool {
    args["add"].is_string() || args["old"].is_string()
}

/// The `memory` tool: `add` saves, `old`/`new` replaces or removes, neither lists.
pub fn run(args: &Value) -> crate::tools::Output {
    run_in(&path(), args)
}

fn run_in(file: &Path, args: &Value) -> crate::tools::Output {
    if let Some(old) = args["old"].as_str() {
        if args["add"].is_string() {
            return crate::tools::Output::error(
                "memory takes `add`, or `old` with `new`, not both: one call changes one entry.",
            );
        }
        return match edit_in(file, old, args["new"].as_str().unwrap_or_default()) {
            Ok(Edited::Replaced { at, old, new }) => {
                let items = list_in(file);
                crate::tools::Output::ok(format!(
                    "Replaced memory {at}: {old} → {new}\nMemory holds {}:\n{}",
                    items.len(),
                    numbered(&items)
                ))
                .brief("replaced")
            }
            Ok(Edited::Removed { at, old }) => {
                let items = list_in(file);
                crate::tools::Output::ok(format!(
                    "Removed memory {at}: {old}\nMemory holds {}:\n{}",
                    items.len(),
                    numbered(&items)
                ))
                .brief("removed")
            }
            Err(e) => crate::tools::Output::error(format!("{e:#}")),
        };
    }
    if let Some(fact) = args["add"].as_str() {
        return match add_in(file, fact) {
            Ok(Saved::Added(at)) => {
                let items = list_in(file);
                crate::tools::Output::ok(format!(
                    "Saved as memory {at} of {}; it goes into every later session's prompt. The user sees it with /memory.\n{}",
                    items.len(),
                    numbered(&items)
                ))
                .brief("saved")
            }
            Ok(Saved::Already(at)) => crate::tools::Output::ok(format!(
                "Already saved as memory {at} of {}; nothing changed.",
                list_in(file).len()
            ))
            .brief("already saved"),
            Err(e) => crate::tools::Output::error(format!("{e:#}")),
        };
    }
    let items = list_in(file);
    if items.is_empty() {
        crate::tools::Output::ok(
            "Memory is empty. `add` saves a fact that should hold in every project.",
        )
        .brief("empty")
    } else {
        crate::tools::Output::ok(format!(
            "Memory holds {}:\n{}",
            items.len(),
            numbered(&items)
        ))
        .brief(format!("{} saved", items.len()))
    }
}

fn list_in(file: &Path) -> Vec<String> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn for_prompt_in(file: &Path) -> Loaded {
    let items = list_in(file);
    let total = items.len();
    let mut sent = Vec::new();
    let mut bytes = 0;
    for item in items {
        if sent.len() == MAX_ITEMS {
            break;
        }
        let next = bytes + entry_bytes(&item);
        if next > MAX_BYTES {
            break;
        }
        bytes = next;
        sent.push(item);
    }
    Loaded { items: sent, total }
}

fn add_in(file: &Path, fact: &str) -> Result<Saved> {
    let fact = normalize(fact);
    if fact.is_empty() {
        bail!("a memory needs some text");
    }
    check_chars(&fact)?;
    with_lock(file, |file| {
        let mut items = list_in(file);
        if let Some(at) = items
            .iter()
            .position(|item| item.eq_ignore_ascii_case(&fact))
        {
            return Ok(Saved::Already(at + 1));
        }
        let bytes = items.iter().map(|item| entry_bytes(item)).sum::<usize>() + entry_bytes(&fact);
        check_room(&items, bytes)?;
        items.push(fact);
        save(file, &items)?;
        Ok(Saved::Added(items.len()))
    })
}

fn forget_in(file: &Path, number: usize) -> Result<(String, usize)> {
    with_lock(file, |file| {
        let mut items = list_in(file);
        if number == 0 || number > items.len() {
            bail!("there is no memory {number}; {} saved", items.len());
        }
        let item = items.remove(number - 1);
        save(file, &items)?;
        Ok((item, items.len()))
    })
}

fn edit_in(file: &Path, old: &str, new: &str) -> Result<Edited> {
    let old = normalize(old);
    let new = normalize(new);
    if old.is_empty() {
        bail!(
            "`old` needs the text of the memory to change; `memory` with no arguments lists them"
        );
    }
    if !new.is_empty() {
        check_chars(&new)?;
    }
    with_lock(file, |file| {
        let mut items = list_in(file);
        let Some(at) = items
            .iter()
            .position(|item| item.eq_ignore_ascii_case(&old))
        else {
            bail!(
                "no memory matches “{old}”. Memory holds{}:\n{}",
                if items.is_empty() { " nothing" } else { "" },
                numbered(&items)
            );
        };
        let stored = items[at].clone();
        if new.is_empty() {
            items.remove(at);
            save(file, &items)?;
            return Ok(Edited::Removed {
                at: at + 1,
                old: stored,
            });
        }
        let bytes = items.iter().map(|item| entry_bytes(item)).sum::<usize>()
            - entry_bytes(&stored)
            + entry_bytes(&new);
        if bytes > MAX_BYTES {
            bail!(
                "memory is full: this edit would make {bytes} bytes of {MAX_BYTES}. Forget one first, with `old` and an empty `new`."
            );
        }
        items[at] = new.clone();
        save(file, &items)?;
        Ok(Edited::Replaced {
            at: at + 1,
            old: stored,
            new,
        })
    })
}

/// One entry's bytes in the file: `- `, its text and the newline.
fn entry_bytes(item: &str) -> usize {
    item.len() + 3
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_chars(fact: &str) -> Result<()> {
    let chars = fact.chars().count();
    if chars > MAX_CHARS {
        bail!(
            "this memory is {chars} characters, over the {MAX_CHARS} one entry may hold; save the short version"
        );
    }
    Ok(())
}

fn check_room(items: &[String], bytes: usize) -> Result<()> {
    if items.len() >= MAX_ITEMS {
        bail!(
            "memory is full: {MAX_ITEMS} entries. Forget one first, with `old` and an empty `new`, or /memory forget N."
        );
    }
    if bytes > MAX_BYTES {
        bail!(
            "memory is full: this entry would make {bytes} bytes of {MAX_BYTES}. Forget one first, with `old` and an empty `new`, or /memory forget N."
        );
    }
    Ok(())
}

/// Runs `change` with the memory's lock held, so two sessions changing it don't lose each
/// other's entry: the lock file never moves, while the memory file is replaced on every save.
fn with_lock<T>(file: &Path, change: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    let lock = file.with_extension("lock");
    if let Some(dir) = lock.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    let handle: File = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&lock)
        .with_context(|| format!("could not open {}", lock.display()))?;
    handle
        .lock()
        .with_context(|| format!("could not lock {}", lock.display()))?;
    change(file)
}

fn save(file: &Path, items: &[String]) -> Result<()> {
    let text: String = items.iter().map(|item| format!("- {item}\n")).collect();
    crate::fs_atomic::write(file, text.as_bytes(), Some(0o600))
        .map_err(|e| anyhow::anyhow!("could not save {}: {e:#}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_temp::TempDir;

    #[test]
    fn facts_are_saved_once_listed_in_order_and_forgotten_by_number() {
        let dir = TempDir::new("killi-memory");
        let file = dir.join("memory.md");
        assert!(list_in(&file).is_empty());
        assert_eq!(
            add_in(&file, "  Uses pnpm,  not npm ").unwrap(),
            Saved::Added(1)
        );
        assert_eq!(
            add_in(&file, "Prefers terse replies").unwrap(),
            Saved::Added(2)
        );
        assert_eq!(
            add_in(&file, "uses pnpm, not npm").unwrap(),
            Saved::Already(1)
        );
        assert_eq!(
            list_in(&file),
            ["Uses pnpm, not npm", "Prefers terse replies"]
        );
        assert_eq!(
            forget_in(&file, 1).unwrap(),
            ("Uses pnpm, not npm".to_string(), 1)
        );
        assert_eq!(forget_in(&file, 1).unwrap().1, 0);
        assert!(
            forget_in(&file, 2)
                .unwrap_err()
                .to_string()
                .contains("no memory 2")
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "");
    }

    #[test]
    fn an_edit_finds_its_entry_by_text_and_leaves_the_others_in_place() {
        let dir = TempDir::new("killi-memory-edit");
        let file = dir.join("memory.md");
        add_in(&file, "Uses pnpm").unwrap();
        add_in(&file, "Prefers terse replies").unwrap();
        add_in(&file, "Runs cargo fmt before commits").unwrap();
        assert_eq!(
            edit_in(&file, "prefers  Terse replies", "Prefers short answers").unwrap(),
            Edited::Replaced {
                at: 2,
                old: "Prefers terse replies".to_string(),
                new: "Prefers short answers".to_string(),
            }
        );
        assert_eq!(
            edit_in(&file, "Uses pnpm", "").unwrap(),
            Edited::Removed {
                at: 1,
                old: "Uses pnpm".to_string()
            }
        );
        assert_eq!(
            list_in(&file),
            ["Prefers short answers", "Runs cargo fmt before commits"]
        );
        let missing = edit_in(&file, "Uses npm", "").unwrap_err().to_string();
        assert!(missing.contains("no memory matches"), "{missing}");
        assert!(missing.contains("1. Prefers short answers"), "{missing}");
        assert!(
            edit_in(&file, "", "anything")
                .unwrap_err()
                .to_string()
                .contains("`old` needs")
        );
    }

    #[test]
    fn the_limits_keep_the_memory_small_enough_to_send_every_time() {
        let dir = TempDir::new("killi-memory-limits");
        let file = dir.join("memory.md");
        let long = "x".repeat(MAX_CHARS + 1);
        assert!(
            add_in(&file, &long)
                .unwrap_err()
                .to_string()
                .contains("over the")
        );
        let full: String = (0..MAX_ITEMS).map(|n| format!("- fact {n}\n")).collect();
        fs::write(&file, full).unwrap();
        assert!(
            add_in(&file, "one more")
                .unwrap_err()
                .to_string()
                .contains("full")
        );
        let near: String = (0..16)
            .map(|n| format!("- {}\n", "y".repeat(MAX_CHARS - 5 + n)))
            .collect();
        fs::write(&file, near).unwrap();
        assert!(
            add_in(&file, &"z".repeat(MAX_CHARS))
                .unwrap_err()
                .to_string()
                .contains("bytes")
        );
    }

    #[test]
    fn a_hand_edited_file_past_the_limits_sends_only_what_fits() {
        let dir = TempDir::new("killi-memory-load");
        let file = dir.join("memory.md");
        let many: String = (0..MAX_ITEMS + 5)
            .map(|n| format!("- fact {n}\n"))
            .collect();
        fs::write(&file, many).unwrap();
        let loaded = for_prompt_in(&file);
        assert_eq!(loaded.total, MAX_ITEMS + 5);
        assert_eq!(loaded.items.len(), MAX_ITEMS);
        assert_eq!(loaded.items[0], "fact 0");
        let big: String = (0..MAX_ITEMS)
            .map(|_| format!("- {}\n", "w".repeat(MAX_CHARS)))
            .collect();
        fs::write(&file, big).unwrap();
        let loaded = for_prompt_in(&file);
        // Each entry takes 243 bytes, so 16 fit under 4096 and the 17th would pass it.
        assert_eq!(loaded.items.len(), 16);
        assert_eq!(loaded.total, MAX_ITEMS);
        let bytes: usize = loaded.items.iter().map(|item| entry_bytes(item)).sum();
        assert!(bytes <= MAX_BYTES, "{bytes}");
    }

    #[test]
    fn the_tool_lists_adds_edits_and_removes_entries() {
        let dir = TempDir::new("killi-memory-tool");
        let file = dir.join("memory.md");
        let saved = run_in(
            &file,
            &serde_json::json!({"add": "Runs cargo fmt before commits"}),
        );
        let again = run_in(
            &file,
            &serde_json::json!({"add": "runs cargo fmt before commits"}),
        );
        let listed = run_in(&file, &serde_json::json!({}));
        let replaced = run_in(
            &file,
            &serde_json::json!({"old": "Runs cargo fmt before commits", "new": "Runs cargo fmt --all"}),
        );
        let removed = run_in(
            &file,
            &serde_json::json!({"old": "Runs cargo fmt --all", "new": ""}),
        );
        let missing = run_in(&file, &serde_json::json!({"old": "Runs npm"}));
        let both = run_in(
            &file,
            &serde_json::json!({"add": "Something", "old": "Runs cargo fmt --all"}),
        );
        let empty = run_in(&file, &serde_json::json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.contains("memory 1 of 1"), "{}", saved.text);
        assert!(saved.text.contains("1. Runs cargo fmt"), "{}", saved.text);
        assert!(
            again.text.contains("Already saved as memory 1"),
            "{}",
            again.text
        );
        assert!(listed.text.contains("Memory holds 1"), "{}", listed.text);
        assert!(
            replaced.text.contains("Replaced memory 1"),
            "{}",
            replaced.text
        );
        assert!(
            replaced.text.contains("Runs cargo fmt --all"),
            "{}",
            replaced.text
        );
        assert!(
            removed.text.contains("Removed memory 1"),
            "{}",
            removed.text
        );
        assert!(
            removed.text.contains("0 saved") || removed.text.contains("holds 0"),
            "{}",
            removed.text
        );
        assert!(
            missing.is_error && missing.text.contains("no memory matches"),
            "{}",
            missing.text
        );
        assert!(
            both.is_error && both.text.contains("not both"),
            "{}",
            both.text
        );
        assert!(empty.text.contains("empty"), "{}", empty.text);
    }
}
