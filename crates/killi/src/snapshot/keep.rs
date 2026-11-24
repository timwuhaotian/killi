//! What undo leaves in place. A file that is in the tree now but not in the snapshot is
//! usually one the turn created, which undo deletes. Two other kinds look the same and
//! predate the turn: a file the snapshot's ignore rules ignored, which the turn un-ignored
//! (a `.gitignore` rewritten without `.env`), and a file left out of the snapshot for its
//! size. Undo can't tell when those were made, so it keeps them. The snapshot's ignore rules
//! include the `.gitignore` files that ignore themselves (`*` in `secrets/.gitignore`, or a
//! root one listing `.gitignore`), which the tree can't hold, so each snapshot hashes those
//! on the side.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};

use super::{BIG_FILE, TakeWork, Taken, base, deadline, git_bytes, paths};

/// The `.gitignore` files the snapshot left out because ignore rules, maybe their own, ignore
/// them, written into the snapshot repo as blobs: each path with its blob id. An ignored
/// folder is listed as itself, not walked, and the `.gitignore` files in one never apply.
pub(super) fn self_ignored(work: &TakeWork) -> Result<Vec<(Vec<u8>, String)>> {
    let listed = git_bytes(
        work,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "--",
            ":(glob)**/.gitignore",
        ],
        None,
    )?;
    let rules: Vec<&[u8]> = paths(&listed)
        .filter(|path| {
            let file = Path::new(OsStr::from_bytes(path));
            file.file_name() == Some(OsStr::new(".gitignore"))
                && fs::symlink_metadata(work.work_tree.join(file))
                    .is_ok_and(|meta| meta.is_file() && meta.len() <= BIG_FILE)
        })
        .collect();
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let mut args: Vec<OsString> = ["hash-object", "-w", "--no-filters", "--"]
        .map(OsString::from)
        .into();
    args.extend(
        rules
            .iter()
            .map(|path| OsStr::from_bytes(path).to_os_string()),
    );
    let hashed = git_bytes(work, &args, None)?;
    let ids: Vec<String> = String::from_utf8_lossy(&hashed)
        .lines()
        .map(String::from)
        .collect();
    if ids.len() != rules.len() {
        bail!(
            "git hash-object printed {} ids for {} files",
            ids.len(),
            rules.len()
        );
    }
    Ok(rules.into_iter().map(<[u8]>::to_vec).zip(ids).collect())
}

/// The paths in tree `now` but not in `snapshot` that undo must leave where they are.
pub(super) fn kept(work: &TakeWork, snapshot: &Taken, now: &str) -> Result<Vec<Vec<u8>>> {
    let added = git_bytes(
        work,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            "-z",
            &snapshot.tree,
            now,
        ],
        None,
    )?;
    if paths(&added).next().is_none() {
        return Ok(Vec::new());
    }
    let ignored = ignored_by(work, snapshot, &added)?;
    let mut kept: Vec<Vec<u8>> = paths(&ignored).map(<[u8]>::to_vec).collect();
    let big: HashSet<&[u8]> = snapshot.big.iter().map(Vec::as_slice).collect();
    let seen: HashSet<Vec<u8>> = kept.iter().cloned().collect();
    kept.extend(
        paths(&added)
            .filter(|path| big.contains(path) && !seen.contains(*path))
            .map(<[u8]>::to_vec),
    );
    Ok(kept)
}

/// Which of `listed` (NUL-separated) the snapshot's `.gitignore` files ignore. Those files
/// are written into a scratch folder in the snapshot repo, where `git check-ignore` reads
/// them; the user's tree is not touched.
fn ignored_by(work: &TakeWork, snapshot: &Taken, listed: &[u8]) -> Result<Vec<u8>> {
    let scratch = work.git_dir.join(format!("undo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    let ignored = check_in(work, snapshot, listed, &scratch);
    let _ = fs::remove_dir_all(&scratch);
    ignored
}

/// Writes the snapshot's `.gitignore` files that can bear on `listed` into `scratch` and asks
/// `git check-ignore` there. Only those in a folder above a listed path can, and they are
/// read in one `git cat-file`.
fn check_in(work: &TakeWork, snapshot: &Taken, listed: &[u8], scratch: &Path) -> Result<Vec<u8>> {
    fs::create_dir_all(scratch)
        .with_context(|| format!("could not create {}", scratch.display()))?;
    let folders: HashSet<&Path> = paths(listed)
        .flat_map(|path| Path::new(OsStr::from_bytes(path)).ancestors().skip(1))
        .collect();
    let applies = |path: &Path| {
        path.file_name() == Some(OsStr::new(".gitignore"))
            && path.components().all(|c| matches!(c, Component::Normal(_)))
            && path.parent().is_some_and(|folder| folders.contains(folder))
    };
    let entries = git_bytes(work, &["ls-tree", "-r", "-z", &snapshot.tree], None)?;
    let mut rules: Vec<(&Path, &str)> = Vec::new();
    for entry in paths(&entries) {
        // `<mode> <type> <object>\t<path>`
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let path = Path::new(OsStr::from_bytes(&entry[tab + 1..]));
        let fields: Vec<&[u8]> = entry[..tab].split(|b| *b == b' ').collect();
        let [mode, kind, object] = fields[..] else {
            continue;
        };
        let regular = mode == b"100644" || mode == b"100755";
        if regular
            && kind == b"blob"
            && applies(path)
            && let Ok(object) = std::str::from_utf8(object)
        {
            rules.push((path, object));
        }
    }
    for (path, object) in &snapshot.ignores {
        let path = Path::new(OsStr::from_bytes(path));
        if applies(path) {
            rules.push((path, object));
        }
    }
    let objects: Vec<&str> = rules.iter().map(|(_, object)| *object).collect();
    for ((path, _), text) in rules.iter().zip(blobs(work, &objects)?) {
        let target = scratch.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, text)
            .with_context(|| format!("could not write {}", target.display()))?;
    }
    let output = deadline::output(
        base(&work.git_dir, scratch).args(["check-ignore", "--no-index", "--stdin", "-z"]),
        Some(listed.to_vec()),
        work.deadline,
        &work.cancel.0,
    )
    .context("git check-ignore")?;
    // 1 means none of them is ignored.
    match output.status.code() {
        Some(0 | 1) => Ok(output.stdout),
        _ => bail!(
            "git check-ignore failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

/// The contents of these blobs, in order, through one `git cat-file --batch`.
fn blobs(work: &TakeWork, objects: &[&str]) -> Result<Vec<Vec<u8>>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = objects.join("\n");
    input.push('\n');
    let printed = git_bytes(work, &["cat-file", "--batch"], Some(input.as_bytes()))?;
    let mut rest = printed.as_slice();
    let mut blobs = Vec::with_capacity(objects.len());
    for object in objects {
        // `<object> blob <size>\n<contents>\n`, or `<object> missing\n`
        let end = rest
            .iter()
            .position(|b| *b == b'\n')
            .context("git cat-file printed less than asked")?;
        let header = String::from_utf8_lossy(&rest[..end]);
        let size = match header.split(' ').collect::<Vec<_>>()[..] {
            [_, "blob", size] => size.parse::<usize>().ok(),
            _ => None,
        };
        let Some(size) = size else {
            bail!("git cat-file could not read {object}: {header}");
        };
        let body = rest
            .get(end + 1..end + 1 + size)
            .context("git cat-file printed less than asked")?;
        blobs.push(body.to_vec());
        rest = rest.get(end + 2 + size..).unwrap_or_default();
    }
    Ok(blobs)
}
