//! Things noted for later: a blocker, a bug seen in passing, an improvement the agent won't
//! do now. One list per project, `later.md` beside its session logs, holding open items
//! only: an item is crossed off and gone, so the list never turns into an archive.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The list's file for the project at `cwd`.
pub fn path(cwd: &Path) -> PathBuf {
    crate::session::project_dir(cwd).join("later.md")
}

/// The open items, oldest first.
pub fn list(cwd: &Path) -> Vec<String> {
    list_in(&path(cwd))
}

/// Adds `note` and says how many items are open.
pub fn add(cwd: &Path, note: &str) -> Result<usize> {
    add_in(&path(cwd), note)
}

/// Removes item `number` (as `/later` counts them, from 1) and gives its text back.
pub fn done(cwd: &Path, number: usize) -> Result<String> {
    done_in(&path(cwd), number)
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

fn add_in(file: &Path, note: &str) -> Result<usize> {
    let mut items = list_in(file);
    items.push(note.split_whitespace().collect::<Vec<_>>().join(" "));
    save(file, &items)?;
    Ok(items.len())
}

fn done_in(file: &Path, number: usize) -> Result<String> {
    let mut items = list_in(file);
    if number == 0 || number > items.len() {
        bail!("there is no item {number}; {} open", items.len());
    }
    let item = items.remove(number - 1);
    save(file, &items)?;
    Ok(item)
}

fn save(file: &Path, items: &[String]) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    let text: String = items.iter().map(|item| format!("- {item}\n")).collect();
    fs::write(file, text).with_context(|| format!("could not write {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_added_in_order_listed_and_crossed_off_by_number() {
        let dir = std::env::temp_dir().join(format!("killi-later-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let file = dir.join("later.md");
        assert!(list_in(&file).is_empty());
        assert_eq!(add_in(&file, "fix the flaky   test\nin ci").unwrap(), 1);
        assert_eq!(add_in(&file, "cache the glob").unwrap(), 2);
        assert_eq!(list_in(&file), ["fix the flaky test in ci", "cache the glob"]);
        assert!(done_in(&file, 3).unwrap_err().to_string().contains("no item 3"));
        assert!(done_in(&file, 0).is_err());
        assert_eq!(done_in(&file, 1).unwrap(), "fix the flaky test in ci");
        assert_eq!(list_in(&file), ["cache the glob"]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "- cache the glob\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
