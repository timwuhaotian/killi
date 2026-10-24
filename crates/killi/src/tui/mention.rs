//! Files named in the prompt with `@`: the workspace listing the `@` menu searches, and the
//! contents of the files a sent prompt names, which go to the model with it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::tools::{MAX_BYTES, is_binary};

/// Entries the listing holds at most, so a huge tree can't stall a keystroke.
const MAX_LISTED: usize = 20_000;

/// The workspace's files and folders under `cwd` as relative paths, folders ending in `/`,
/// honoring .gitignore (in a git repository or not) and leaving out `.git`; sorted.
pub fn list(cwd: &Path) -> Vec<String> {
    let mut paths: Vec<String> = ignore::WalkBuilder::new(cwd)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .flatten()
        .filter(|e| e.depth() > 0)
        .take(MAX_LISTED)
        .map(|e| {
            let path = e.path().strip_prefix(cwd).unwrap_or(e.path()).to_string_lossy().into_owned();
            if e.file_type().is_some_and(|t| t.is_dir()) { format!("{path}/") } else { path }
        })
        .collect();
    paths.sort();
    paths
}

/// `text` with the contents of the files it names with `@` after it, in a system reminder,
/// or `text` as it is when it names none that can be read.
pub fn expand(cwd: &Path, text: &str) -> String {
    match attachment(cwd, text) {
        Some(block) => format!("{text}\n\n{block}"),
        None => text.to_string(),
    }
}

/// A system reminder holding the files `text` names with `@`, each once, in the order named;
/// None when it names no readable file. Folders and missing paths are left to the model, which
/// has the text; a binary file is named without its bytes; a long one is cut at the `read`
/// tool's limit.
pub fn attachment(cwd: &Path, text: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut seen = Vec::new();
    for (name, full) in mentions(cwd, text) {
        if seen.contains(&name) || !full.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(&full) else { continue };
        seen.push(name.clone());
        if is_binary(&bytes) {
            parts.push(format!("<file path=\"{name}\" binary=\"true\" bytes=\"{}\"/>", bytes.len()));
            continue;
        }
        let mut content = String::from_utf8_lossy(&bytes).into_owned();
        if content.len() > MAX_BYTES {
            let mut cut = MAX_BYTES;
            while !content.is_char_boundary(cut) {
                cut -= 1;
            }
            content.truncate(cut);
            content.push_str(&format!("\n[cut here at {} of {} bytes; the read tool shows the rest]", cut, bytes.len()));
        }
        parts.push(format!("<file path=\"{name}\">\n{}\n</file>", content.trim_end_matches('\n')));
    }
    (!parts.is_empty()).then(|| format!("<system-reminder>The files the user named with @, as they are now:\n\n{}</system-reminder>", parts.join("\n\n")))
}

/// The `@path` words of `text`, as typed and resolved against `cwd`. Punctuation that ends a
/// sentence after the path is left out unless the path exists with it.
fn mentions(cwd: &Path, text: &str) -> Vec<(String, PathBuf)> {
    text.split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .filter(|path| !path.is_empty())
        .map(|path| {
            let full = cwd.join(path);
            if full.exists() {
                return (path.to_string(), full);
            }
            let bare = path.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
            (bare.to_string(), cwd.join(bare))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("killi-mention-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/tui")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/tui/mod.rs"), "pub mod menu;\n").unwrap();
        fs::write(root.join("logo.png"), b"\x89PNG\0\0\0").unwrap();
        fs::write(root.join("target/out"), "built").unwrap();
        root
    }

    #[test]
    fn the_listing_has_files_and_folders_but_not_ignored_ones() {
        let root = workspace("list");
        assert_eq!(list(&root), [".gitignore", "logo.png", "src/", "src/main.rs", "src/tui/", "src/tui/mod.rs"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn named_files_follow_the_text_in_a_reminder() {
        let root = workspace("expand");
        let sent = expand(&root, "fix @src/main.rs, see @src/tui/mod.rs. and @src/main.rs");
        let (text, block) = sent.split_once("\n\n").unwrap();
        assert_eq!(text, "fix @src/main.rs, see @src/tui/mod.rs. and @src/main.rs");
        assert_eq!(
            block,
            "<system-reminder>The files the user named with @, as they are now:\n\n<file path=\"src/main.rs\">\nfn main() {}\n</file>\n\n<file path=\"src/tui/mod.rs\">\npub mod menu;\n</file></system-reminder>"
        );
        assert_eq!(expand(&root, "look in @src/ and @nope.rs or me@home"), "look in @src/ and @nope.rs or me@home");
        assert!(expand(&root, "@logo.png").ends_with("<file path=\"logo.png\" binary=\"true\" bytes=\"7\"/></system-reminder>"));
        fs::write(root.join("big.txt"), "x".repeat(MAX_BYTES + 10)).unwrap();
        let sent = expand(&root, "@big.txt");
        assert!(sent.contains(&format!("\n[cut here at {MAX_BYTES} of {} bytes; the read tool shows the rest]\n</file>", MAX_BYTES + 10)));
        fs::remove_dir_all(&root).unwrap();
    }
}
