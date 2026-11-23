use super::*;

fn bash_allowed(command: &str, extra: &[String]) -> bool {
    bash_refused(command, extra).is_none()
}

#[test]
fn read_only_commands_pass() {
    for command in [
        "ls -la",
        "cd sub && rg slugify src | head -20",
        "cd /tmp; cat notes.txt | wc -l",
        "rg slugify src | head -20",
        "git log --oneline -5 && git status",
        "cargo check 2>&1 | tail -30",
        "find . -name '*.rs'",
        "sed -n 1,40p src/lib.rs",
        "killi sessions",
        "killi ask 4321 \"which files are you changing?\"",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn writes_are_refused() {
    for command in [
        "rm -rf target",
        "echo hi > notes.txt",
        "cat a | tee b",
        "find . -name '*.tmp' -delete",
        "sed -i 's/a/b/' x",
        "git checkout -- .",
        "ls $(touch x)",
        "cargo test",
        "npm install",
        "killi tell 4321 'I pushed the base'",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn the_refusal_names_the_segment() {
    assert_eq!(
        bash_refused("cd sub && rm x | head -1", &[]),
        Some("rm x".to_string())
    );
    assert_eq!(
        bash_refused("ls | tee out", &[]),
        Some("tee out".to_string())
    );
    assert_eq!(
        bash_refused("echo hi > notes.txt", &[]).as_deref(),
        Some("echo hi > notes.txt")
    );
    assert_eq!(bash_refused("cd sub && ls 2>&1 | head -1", &[]), None);
}

#[test]
fn config_can_allow_more() {
    assert!(bash_allowed("make -n all", &["make -n".to_string()]));
}

#[test]
fn quoted_separators_are_text() {
    // Session 1795366373-9223: plan mode named `run` and `approval`, the halves of
    // a quoted alternation, and the counts were dropped.
    for command in [
        "grep -E 'agent|run|approval' src",
        "rg 'a;b' src",
        r"echo $'a|b'",
        r#"echo "a & b" "#,
        "echo 'a\' | wc -l",
        "echo a \\| b",
        r#"echo "a\" | rm x""#,
        r#"echo "it's" "#,
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "echo 'a|b' | rm x",
        r"echo $'a\'' | rm x",
        "echo 'a;b'; rm x",
        "echo 'a' & rm x",
        "echo 'a'\nrm x",
        "echo '$(rm x)'",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    assert_eq!(
        bash_refused("echo 'a|b' | rm x", &[]),
        Some("rm x".to_string())
    );
}

#[test]
fn git_global_options_and_xargs_read_through() {
    for command in [
        "git -C /repo ls-files | xargs wc -l",
        "git -C /repo log --oneline -5",
        "git --no-pager -C sub diff HEAD",
        "git --git-dir=.git --work-tree=. status",
        "git -Csub rev-parse HEAD",
        "ls | xargs wc -l",
        "ls | xargs -0 -I{} wc -l {}",
        "ls | xargs -n 1 -P 4 wc -c",
        "ls | xargs --max-args=1 sort | uniq -c",
        "ls | xargs",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "git -C sub branch -D old",
        "git -C sub checkout -- .",
        "git -C sub diff --output=p HEAD",
        "git -c alias.x='!rm -r /' log",
        "git -C",
        "xargs rm",
        "xargs -I{} rm {}",
        "xargs -e rm",
        "xargs xargs rm",
        "xargs find . -delete",
        "ls | xargs sort -o out",
        "xargs sh -c 'rm x'",
        "xargs 'rm' -rf",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn writers_behind_read_only_names_are_refused() {
    for command in [
        "fd . -x rm",
        "fd . --exec rm {}",
        "rg --pre cat pattern",
        "sort -o out f",
        "sort --output=out f",
        "sort -ro out f",
        "uniq in out",
        "tree -o out",
        "git branch -D old",
        "git branch -m old new",
        "git branch feature",
        "git tag v1.0",
        "git tag -a v1.0 -m x",
        "git tag -d v1.0",
        "git diff --output=patch HEAD",
        "cargo clippy --fix",
        "cargo check --fix",
        "sed -n '/x/w out' f",
        "sed -n 's/a/b/w out' f",
        "sed -n '1w out' f",
        "sed -n 's/a/b/w' f",
        "sed -n 's|a|b|w out' f",
        "git branch --set-upstream-to=origin/main",
        "git branch -u origin/main",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "fd . -t f",
        "rg pattern",
        "sort f | uniq -c",
        "uniq f",
        "uniq -s 2 f",
        "tree -L 2",
        "git branch",
        "git branch -a",
        "git branch --contains HEAD",
        "git branch --list 'feat/*'",
        "git tag -l --contains HEAD",
        "git tag --contains HEAD",
        "git tag",
        "git tag -l 'v*'",
        "git diff HEAD",
        "cargo clippy -- -D warnings",
        "sed -n 1,40p src/lib.rs",
        "sed -n '/x/p' f",
        "sed -n 's/a/w/' f",
        "sed -n 's/a/b/p' words.txt",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}
