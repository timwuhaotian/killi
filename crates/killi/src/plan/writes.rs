//! Flags and subcommands that make a command on the read-only list write a file or run a
//! program. The words have their quotes removed already.

use super::sed::sed_writes;

/// Flags and subcommands that write behind a read-only command name: `sort -o`, `git tag v1`,
/// `sed -n ... w out`, or run a program the command line names: `rg --pre`, `git grep -O`,
/// `go vet -vettool`. The whole segment is refused when one shows up. `opaque` says, word by
/// word, that bash may hand the command something else there (see `Word::opaque`).
pub(super) fn writes_behind_a_read_only_name(words: &[&str], opaque: &[bool]) -> bool {
    let args = || words.iter().skip(1);
    let has = |flags: &[&str]| args().any(|w| flags.iter().any(|f| long_flag(w, f)));
    let cluster =
        |flags: &str, takes_value: &str| args().any(|w| short_cluster(w, flags, takes_value));
    match words.first() {
        Some(&"find") => args().any(|w| {
            matches!(
                *w,
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        Some(&"fd") => {
            has(&["--exec", "--exec-batch"])
                || args().any(|w| *w == "-x" || *w == "-X")
                || cluster("xX", "teEdjSco")
        }
        Some(&"rg") => has(&["--pre", "--hostname-bin"]),
        Some(&"sort") => has(&["--output", "--compress-program"]) || cluster("o", "kStT"),
        Some(&"tree") => has(&["--output"]) || cluster("oR", ""),
        Some(&"uniq") => uniq_has_output(words) || opaque.iter().skip(1).any(|&o| o),
        Some(&"sed") => sed_writes(words, opaque),
        Some(&"xxd") => xxd_writes(words) || opaque.iter().skip(1).any(|&o| o),
        // `env` with a command runs it; alone it prints the environment.
        Some(&"env") => words.len() > 1,
        Some(&"git") => git_writes(words),
        Some(&"cargo") => {
            (matches!(words.get(..2), Some(["cargo", "clippy" | "check"])) && has(&["--fix"]))
                || has(&["--config"])
                || args().any(|w| *w == "--target-dir" || w.starts_with("--target-dir="))
                || cluster("Z", "")
                || cargo_passes_on(words)
        }
        // Go's flags take one dash or two and no abbreviations. Flags for the compiler,
        // assembler and linker can name an output file; `-mod` and `-modfile` let go rewrite
        // go.mod; `-pkgdir` writes installed packages.
        Some(&"go") => args().any(|w| {
            let name = w.trim_start_matches('-');
            let name = name.split('=').next().unwrap_or(name);
            w.starts_with('-')
                && matches!(
                    name,
                    "toolexec"
                        | "vettool"
                        | "exec"
                        | "gcflags"
                        | "asmflags"
                        | "ldflags"
                        | "gccgoflags"
                        | "pkgdir"
                        | "modfile"
                        | "mod"
                )
        }),
        // pnpm runs the JavaScript a pnpmfile names, and takes any setting as `--config.x`.
        // It expands an unambiguous abbreviation, but `--global` is a flag of its own. Plain
        // `pnpm ls` runs the project's own `.pnpmfile.cjs`, as `cargo check` runs its build
        // scripts: that is the project's code, not a program the command line picks.
        Some(&"pnpm") => args().any(|w| {
            let name = w.split('=').next().unwrap_or(w);
            (name.len() >= "--pn".len() && "--pnpmfile".starts_with(name))
                || (name.len() > "--global".len() && "--global-pnpmfile".starts_with(name))
                || w.starts_with("--config.")
        }),
        // npm writes its debug log, and `view` its cache, wherever these point.
        Some(&"npm") => has(&["--logs-dir", "--cache"]),
        Some(&"xcodebuild") => xcodebuild_writes(words),
        Some(&"less" | &"more") => {
            has(&[
                "--log-file",
                "--LOG-FILE",
                "--lesskey-file",
                "--lesskey-src",
                "--lesskey-content",
            ]) || cluster("oOk", "bhjpPtTxyzD#")
                || args().any(|w| w.starts_with('+'))
        }
        Some(&"tsc") => tsc_writes(words),
        Some(&"file") => has(&["--compile"]) || cluster("C", "efFmP"),
        Some(&"printf") => words.get(1).is_some_and(|w| w.starts_with("-v")),
        _ => false,
    }
}

/// `--name` or `--name=…`, or an unambiguous abbreviation of it such as `--out=f`, which
/// getopt_long, git's option parser and less all accept.
pub(super) fn long_flag(word: &str, full: &str) -> bool {
    let name = word.split('=').next().unwrap_or(word);
    name.len() > 2 && name.starts_with("--") && full.starts_with(name)
}

/// A combined short flag like `-nO`: `-`, not `--`, read letter by letter. True when a letter
/// in `flags` comes before any letter in `takes_value`, whose value is the rest of the word.
fn short_cluster(word: &str, flags: &str, takes_value: &str) -> bool {
    let Some(letters) = word.strip_prefix('-') else {
        return false;
    };
    if letters.starts_with('-') {
        return false;
    }
    for letter in letters.chars() {
        if flags.contains(letter) {
            return true;
        }
        if takes_value.contains(letter) {
            return false;
        }
    }
    false
}

/// A combined short flag like `-ro`: `-`, not `--`, with `flag` among the letters.
fn short_has(word: &str, flag: char) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word[1..].contains(flag)
}

/// `uniq` with an input and an output file writes the second one. Flags that take a value
/// (`-f`, `-s`, `-w`) don't count their value as a file; past `--` every word is a file.
fn uniq_has_output(words: &[&str]) -> bool {
    let mut operands = 0;
    let mut skip_next = false;
    let mut options_done = false;
    for word in words.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *word == "--" && !options_done {
            options_done = true;
            continue;
        }
        if !options_done && word.starts_with('-') && word.len() > 1 {
            skip_next = matches!(
                *word,
                "-f" | "-s" | "-w" | "--skip-fields" | "--skip-chars" | "--check-chars"
            );
            continue;
        }
        operands += 1;
    }
    operands >= 2
}

/// `xxd` with a second file writes it, and `-r` (`-revert`) turns a dump back into bytes.
/// xxd matches flags by their first letters. `-c`, `-g`, `-l`, `-o`, `-s`, `-n` and `-R` take
/// a value, the next word when given alone or under their long names (`-cols 8`); counting a
/// value as a file only refuses more.
fn xxd_writes(words: &[&str]) -> bool {
    let mut operands = 0;
    let mut options_done = false;
    let mut args = words.iter().skip(1);
    while let Some(&word) = args.next() {
        if options_done || word == "-" || !word.starts_with('-') {
            operands += 1;
            continue;
        }
        if word == "--" {
            options_done = true;
            continue;
        }
        if word.starts_with("-r") {
            return true;
        }
        let takes_next = matches!(
            word,
            "-c" | "-cols"
                | "-g"
                | "-groupsize"
                | "-l"
                | "-len"
                | "-o"
                | "-offset"
                | "-s"
                | "-seek"
                | "-n"
                | "-name"
                | "-R"
        );
        if takes_next {
            args.next();
        }
    }
    operands > 1
}

/// What cargo hands on past `--`: rustfmt reads it as options that can write files, and the
/// compiler as flags that can name an output (`--emit=metadata=/path`). Only lint levels
/// (`-D warnings`, `--allow=x`) pass, and nothing for `cargo fmt`.
fn cargo_passes_on(words: &[&str]) -> bool {
    let Some(dashes) = words.iter().position(|w| *w == "--") else {
        return false;
    };
    if words.get(1) == Some(&"fmt") {
        return true;
    }
    let lint = |value: &str| !value.is_empty() && !value.starts_with('-');
    let mut rest = words[dashes + 1..].iter();
    while let Some(&word) = rest.next() {
        if matches!(
            word,
            "-D" | "-W" | "-A" | "-F" | "--deny" | "--warn" | "--allow" | "--forbid"
        ) {
            if !rest.next().is_some_and(|value| lint(value)) {
                return true;
            }
            continue;
        }
        let attached = ["-D", "-W", "-A", "-F"]
            .iter()
            .chain(&["--deny=", "--warn=", "--allow=", "--forbid="])
            .any(|flag| word.strip_prefix(flag).is_some_and(lint));
        if !attached {
            return true;
        }
    }
    false
}

/// `xcodebuild -list` with anything but the flags that pick what to list: build actions and
/// the `-…Path` flags write.
fn xcodebuild_writes(words: &[&str]) -> bool {
    let mut args = words.iter().skip(1);
    while let Some(&word) = args.next() {
        match word {
            "-list" | "-json" => {}
            "-project" | "-workspace" => {
                args.next();
            }
            _ => return true,
        }
    }
    false
}

/// `tsc --noEmit` that writes anyway: a trace or profile, an incremental build-info file,
/// `--init`'s tsconfig.json, or `--noEmit false`. tsc reads option names in any case and
/// with one dash or two.
fn tsc_writes(words: &[&str]) -> bool {
    let name = |w: &str| {
        w.strip_prefix('-').map(|w| {
            let w = w.strip_prefix('-').unwrap_or(w);
            w.split('=').next().unwrap_or(w).to_ascii_lowercase()
        })
    };
    words.iter().enumerate().skip(1).any(|(at, w)| {
        let Some(name) = name(w) else { return false };
        matches!(
            name.as_str(),
            "generatetrace"
                | "generatecpuprofile"
                | "incremental"
                | "tsbuildinfofile"
                | "build"
                | "b"
                | "init"
        ) || (name == "noemit"
            && (w.to_ascii_lowercase().ends_with("=false")
                || words
                    .get(at + 1)
                    .is_some_and(|next| next.eq_ignore_ascii_case("false"))))
    })
}

/// A `--git-dir` before git's subcommand that names anything but a `.git` folder. A bare
/// repository committed to the project brings its own config, and `core.fsmonitor` there
/// runs a program on `git status`. A path ending in `.git` can't be committed.
pub(super) fn foreign_git_dir(words: &[String]) -> bool {
    if words.first().map(String::as_str) != Some("git") {
        return false;
    }
    let mut at = 1;
    while let Some(word) = words.get(at) {
        let dir = match word.as_str() {
            "--git-dir" => words.get(at + 1).map(String::as_str),
            _ => word.strip_prefix("--git-dir="),
        };
        if let Some(dir) = dir {
            let dir = dir.trim_end_matches('/');
            return !(dir == ".git" || dir.ends_with("/.git"));
        }
        if !word.starts_with('-') {
            return false;
        }
        at += if matches!(word.as_str(), "-C" | "--work-tree" | "-c") {
            2
        } else {
            1
        };
    }
    false
}

/// `git branch` creating, deleting or renaming a branch, `git tag` creating or deleting one,
/// `git remote` beyond a listing, `git grep -O` opening files in a program, and `--output=`
/// on any subcommand: all write or run. Bare `git branch`, `git tag` and listings stay allowed.
fn git_writes(words: &[&str]) -> bool {
    if words.len() < 2 || words[0] != "git" {
        return false;
    }
    if words.iter().skip(2).any(|w| long_flag(w, "--output")) {
        return true;
    }
    match words[1] {
        "reflog" => words[2..]
            .iter()
            .any(|w| matches!(*w, "expire" | "delete" | "drop" | "write")),
        "remote" => !matches!(words[2..], [] | ["-v" | "--verbose"] | ["get-url", ..]),
        "grep" => words
            .iter()
            .skip(2)
            .any(|w| long_flag(w, "--open-files-in-pager") || short_cluster(w, "O", "efABCm")),
        "branch" if words.len() > 2 => {
            let takes_value = [
                "--contains",
                "--no-contains",
                "--merged",
                "--no-merged",
                "--points-at",
                "--sort",
                "--format",
                "--set-upstream-to",
            ];
            let writes = [
                "--force",
                "--delete",
                "--move",
                "--copy",
                "--set-upstream-to",
                "--unset-upstream",
                "--edit-description",
            ];
            let listed = words
                .iter()
                .skip(2)
                .any(|w| *w == "-l" || *w == "--list" || short_has(w, 'l'));
            let mut skip_next = false;
            for word in words.iter().skip(2) {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if writes.iter().any(|f| long_flag(word, f)) {
                    return true;
                }
                if takes_value.iter().any(|f| long_flag(word, f)) {
                    skip_next = !word.contains('=');
                    continue;
                }
                if word.starts_with('-')
                    && !word.starts_with("--")
                    && "dDmMcCfu".chars().any(|f| short_has(word, f))
                {
                    return true;
                }
                if !word.starts_with('-') && !listed {
                    return true;
                }
            }
            false
        }
        "tag" if words.len() > 2 => {
            // List filters imply `--list`, so `--contains HEAD` lists just as `-l` does.
            let listed = words.iter().skip(2).any(|w| {
                matches!(*w, "-l" | "--list")
                    || short_has(w, 'l')
                    || [
                        "--contains",
                        "--no-contains",
                        "--merged",
                        "--no-merged",
                        "--points-at",
                    ]
                    .contains(&w.split('=').next().unwrap_or(w))
            });
            words.iter().skip(2).any(|w| {
                let flag = w.split('=').next().unwrap_or(w);
                if matches!(
                    flag,
                    "-l" | "--list"
                        | "-n"
                        | "--sort"
                        | "--format"
                        | "--contains"
                        | "--no-contains"
                        | "--merged"
                        | "--no-merged"
                        | "--points-at"
                        | "--column"
                        | "--no-column"
                ) || (flag == "-n"
                    || (flag.starts_with("-n") && flag[2..].chars().all(|c| c.is_ascii_digit())))
                {
                    return false;
                }
                if listed && !w.starts_with('-') {
                    return false;
                }
                if listed && "dasufme".chars().any(|f| short_has(w, f)) {
                    return true;
                }
                !listed
            })
        }
        _ => false,
    }
}
