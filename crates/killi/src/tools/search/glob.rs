use super::*;

pub(super) fn glob_blocking(cwd: &Path, root: &Path, args: &Value) -> Output {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Output::error("glob needs `pattern`.");
    };
    let matcher = match GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(glob) => glob.compile_matcher(),
        Err(e) => return Output::error(format!("Invalid glob `{pattern}`: {e}")),
    };
    let walk = match walker(root, None) {
        Ok(walk) => walk,
        Err(message) => return Output::error(message),
    };
    let mut found = Vec::new();
    let mut more = false;
    let mut errors = SearchErrors::default();
    let mut scanned = 0;
    let mut scan_limited = false;
    for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = walk_error_path(&error).unwrap_or(root).to_path_buf();
                errors.record(cwd, &path, error);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Err(error) = entry.metadata() {
            errors.record(cwd, entry.path(), error);
            continue;
        }
        if scanned == MAX_SCAN_FILES {
            scan_limited = true;
            break;
        }
        scanned += 1;
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(relative) {
            if found.len() == MAX_FILES {
                more = true;
                break;
            }
            found.push(
                entry
                    .path()
                    .strip_prefix(cwd)
                    .unwrap_or(entry.path())
                    .display()
                    .to_string(),
            );
        }
    }
    let incomplete = scan_limited || errors.count > 0;
    if found.is_empty() {
        let mut text = format!(
            "No files match `{pattern}` under {}.",
            display_root(cwd, root)
        );
        if scan_limited {
            text.push_str(&format!("\n[Search incomplete: internal {MAX_SCAN_FILES} files scan limit reached; narrow the pattern or path.]"));
        }
        if let Some(note) = errors.note() {
            text.push_str(&format!("\n[{note}]"));
        }
        return if incomplete {
            Output::error(text)
        } else {
            Output::ok(text)
        };
    }
    let count = found.len();
    let mut out = found.join("\n");
    if more {
        out.push_str(&format!(
            "\n[{MAX_FILES} results limit reached; narrow the pattern or path.]"
        ));
    }
    if scan_limited {
        out.push_str(&format!("\n[Search incomplete: internal {MAX_SCAN_FILES} files scan limit reached; narrow the pattern or path.]"));
    }
    if let Some(note) = errors.note() {
        out.push_str(&format!("\n[{note}]"));
    }
    Output::ok(out).brief(format!(
        "{count}{} file{}",
        if more { "+" } else { "" },
        if count == 1 { "" } else { "s" }
    ))
}

fn display_root(cwd: &Path, root: &Path) -> String {
    match root.strip_prefix(cwd) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".into(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => root.display().to_string(),
    }
}
