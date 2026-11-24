use super::*;

fn bash_allowed(command: &str, extra: &[String]) -> bool {
    bash_refused(command, extra).is_none()
}

/// The words plan mode checks in one segment, braces expanded.
fn expanded(segment: &str) -> Option<Vec<String>> {
    let budget = &mut braces::Budget::new(segment.len());
    Some(
        words::words(segment, budget)?
            .into_iter()
            .map(|w| w.text)
            .collect(),
    )
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
        "ls | xargs --max-args=1 wc -l | sort",
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
        "ls | xargs sort",
        "echo -delete | xargs find .",
        "echo --pre=sh | xargs rg x",
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

#[test]
fn quotes_come_off_before_the_checks() {
    for command in [
        "find . -name '*.tmp' '-delete'",
        r#"find . -name x "-exec" rm {} +"#,
        "sort '-o' out in",
        "fd x '-x' rm",
        "rg '--pre=sh' foo",
        "sed -n '-i' s/a/b/ f",
        "git diff '--output=x'",
        r#"find . -de""lete"#,
        r"find . -del\ete",
        "find . $'-delete'",
        r"find . $'\x2ddelete'",
        "find . -{delete,name} x",
        "sort -{n..o} out f",
        "find . ${X:--delete}",
        "echo -delete; find . $_",
        "printf -v X %s -delete; find . $X",
        "find . -name 'x",
        "rg foo *",
        "sort -* f",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "grep -E 'a\\|b' src",
        "rg 'foo bar' src",
        "git log --format='%h %s' -5",
        "find . -name '*.rs'",
        "sed -n '1,20p' src/lib.rs",
        r#""ls" -la"#,
        "ls src/{plan,agent}.rs",
        "wc -l src/*.rs",
        "cat *.md",
        "rg foo -- *",
        "grep -rn 'fn ' --include='*.rs' .",
        "rg '-> Result' src",
        "grep -n '<<<<<<<\\|>>>>>>>' f",
        "ls \"$HOME/.killi\" $HOME",
        "cargo check 2>&1 | tail -3; echo \"exit ${PIPESTATUS[0]}\"",
        "git -C sub remote -v 2>/dev/null",
        "find . -name '*.rs' | xargs wc -l",
        "ls | xargs -I{} grep -l foo {}",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn words_are_what_bash_passes() {
    let words = expanded;
    assert_eq!(
        words(r#"a 'b c' "d\"e" f\ g -de""lete $'h\ti'"#).unwrap(),
        ["a", "b c", "d\"e", "f g", "-delete", "h\ti"]
    );
    assert_eq!(
        words("x{a,b}y {1..3} {}").unwrap(),
        ["xay", "xby", "1", "2", "3", "{}"]
    );
    assert_eq!(words("'{a,b}'").unwrap(), ["{a,b}"]);
    assert_eq!(words(r#""a\|b" 'a\|b'"#).unwrap(), [r"a\|b", r"a\|b"]);
    let home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(words("$HOME").unwrap(), std::slice::from_ref(&home));
    assert_eq!(words("\"${HOME}/x\"").unwrap(), [format!("{home}/x")]);
    for refused in [
        "'open",
        "\"open",
        "a > b",
        "`id`",
        "$(id)",
        "${X:-y}",
        "$_",
        "$@",
        "{1..99999}",
    ] {
        assert!(words(refused).is_none(), "{refused}");
    }
}

#[test]
fn read_only_names_that_write_are_refused() {
    for command in [
        "git log -p --output=F",
        "git show --output=F",
        "git show HEAD --out=F",
        "git remote add evil https://x",
        "git remote -v add evil https://x",
        "git remote set-url origin x",
        "find . -fls F",
        "find . -fprintf F %p",
        "find . -fprint0 F",
        "sed -n --in-place s/a/b/ f",
        "sed -n --in=bak s/a/b/ f",
        "sed -ni s/a/b/ f",
        "sed -n -I '' s/a/b/ f",
        r#"sed -n "p;w out" f"#,
        "sed -n 's/a;b/c/w out' f",
        "sed -n '/[/]/w out' f",
        "sed -n -e p -e 'W out' f",
        "sed -n '1{p;w out' f",
        "less -o F f",
        "less -SoF f",
        "less --log-file=F f",
        "less --log=F f",
        "tsc --noEmit --generateTrace D",
        "tsc --noEmit --generatetrace D",
        "tsc --noEmit --init",
        "tsc --noEmit false",
        "file -C -m magic",
        "file -bC",
        "tree -ao out",
        "sort --o=out f",
        "fd . -Hx rm",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "git remote",
        "git remote -v",
        "git remote get-url origin",
        "git log --output-indicator-new=+ -p",
        "less -R f",
        "tsc --noEmit -p tsconfig.json",
        "file -b x",
        "tree -a -L 2",
        "fd -tx foo",
        "sed -n '/fn main/,/^}/p' src/main.rs",
        "sed -n -e 1p -e '$p' f",
        "sed -n '1~3p;$=' f",
        "sed -n 's/[0-9]/N/gp' f",
        "sed -n '/x/{p;q}' f",
        "sed -n 's/a/b/gp' window.rs",
        "sed -n '2,8p' src/web/window.rs",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn read_only_names_that_run_programs_are_refused() {
    for command in [
        "git grep -O'touch pwn' foo",
        "git grep -nO'touch pwn' foo",
        "git grep --open-files-in-pager=x foo",
        "git grep --open-files-in=x foo",
        "git --git-dir=evil.git status",
        "git --git-dir evil log",
        "git -C sub --git-dir=../bare status",
        "sed -n '1e touch pwn' f",
        "sed -n 's/x/touch pwn/e' f",
        "sed -n -f script.sed f",
        "sed -n --file=script.sed f",
        r#"cargo check --config 'build.rustc-wrapper="/tmp/x"'"#,
        "cargo clippy -Zunstable-options",
        "cargo tree -Z avoid-dev-deps",
        "go vet -vettool=/tmp/evil ./...",
        "go list -toolexec=/tmp/evil -export ./...",
        "go list --toolexec /tmp/evil ./...",
        "sort --compress-program=prog f",
        "sort --compress=prog f",
        "less -k evil.keys f",
        "less --lesskey-src=evil f",
        "less '+!touch pwn' f",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "git --git-dir=.git --work-tree=. status",
        "git --git-dir=sub/.git/ log --oneline",
        "git grep -n foo",
        "git grep -e Oops",
        "go vet ./...",
        "go list -m all",
        "cargo check -p killi",
        "sort -k2 -t: f",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn variables_bash_sets_itself_are_unseen() {
    for command in [
        "find . $BASH_COMMAND -name 'x -o -delete -o -name y'",
        "find -name $BASH_COMMAND -o -name 'x -o -delete -o -name y'",
        "find . -name x $BASH_EXECUTION_STRING",
        "find . \"$BASH_COMMAND\"",
        "find . ${BASH_COMMAND}",
        "find $PWD -name x",
        "sort $X f",
        "$X -la",
        "ls$X",
        "git $X",
        "xargs $X",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "echo $HOME",
        "echo $PATH",
        "echo \"PATH=$PATH\"",
        "cat $BASH_COMMAND",
        "find \"$PWD\" -name '*.rs'",
        "ls \"$TMPDIR\"",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
    assert_eq!(
        words::unquote_words("a $BASH_COMMAND").unwrap(),
        ["a", "$BASH_COMMAND"]
    );
}

#[test]
fn a_command_name_matches_word_by_word() {
    for command in [
        "'cat /x' README",
        "'git log' --x",
        "\"ls -la\"",
        "git 'log --output=x'",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    assert!(!bash_allowed("'make -n' all", &["make -n".to_string()]));
    assert!(!bash_allowed("rm x", &[String::new()]));
    assert!(bash_allowed("git 'log' -1", &[]));
}

#[test]
fn comments_and_heredocs_hide_nothing() {
    for command in [
        "ls # what's here\nfind . -delete # it's done",
        "cat <<EOF\ncat '\nEOF\nrm -f a #'",
        "cat <<EOF\nhi\nEOF",
        "cat <<'EOF'\nhi\nEOF",
        "cat <<-EOF\n\thi\n\tEOF",
        "git commit -F - <<EOF\nFix the parser's bug\nEOF\ngit checkout main",
        "ls #\nrm x",
        "ls;#x\nrm x",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    assert_eq!(
        bash_refused("cat <<EOF\ncat '\nEOF\nrm -f a #'", &[]).as_deref(),
        Some("cat <<EOF")
    );
    for command in [
        "ls # list",
        "ls # what's here",
        "# where are we\npwd",
        "ls a#b",
        "echo $# 'x'",
        "grep -n '#' f",
        "cat <<<'hi'",
        "cat <<< \"$HOME\"",
        "wc -l < f",
        "sort <f",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
    assert_eq!(
        segments("git commit -F - <<'EOF'\nit's\nEOF\ngit stash"),
        ["git commit -F - <<'EOF'", "git stash"]
    );
    assert_eq!(segments("ls # a; b | c\npwd"), ["ls ", "", "pwd"]);
    assert_eq!(
        words::unquote_words("sed -n p <in 2<&1 <<<'x y'").unwrap(),
        ["sed", "-n", "p"]
    );
}

#[test]
fn xargs_putting_input_in_the_command_name_is_refused() {
    for command in [
        "echo rm | xargs -J cat cat -f zz",
        "echo rm | xargs -I cat cat x",
        "echo rm | xargs -Icat cat x",
        "echo rm | xargs -I a cat x",
        "echo rm | xargs -i{} {}",
        "echo rm | xargs -i x",
        "echo rm | xargs --replace=cat cat",
        "echo rm | xargs --repl {} x",
        "echo rm | xargs -0I c cat",
        "echo rm | xargs -I '' cat",
        "echo f | xargs -0n cat rm",
        "echo f | xargs -n cat rm",
        "echo f | xargs --max-args cat rm",
        "echo f | xargs --arg-file cat rm",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "ls | xargs -I{} grep -l foo {}",
        "ls | xargs -I % cat %",
        "ls | xargs -0n1 wc -l",
        "ls | xargs -n 1 -P 4 wc -c",
        "ls | xargs --max-args=1 wc -l",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn huge_brace_words_refuse_fast_without_deep_recursion() {
    let inputs = [
        format!("cat {}", "{a,b}".repeat(200_000)),
        format!("cat {}", "{1..1}".repeat(200_000)),
        format!("cat {}", "{".repeat(500_000)),
        format!("cat {}a,b{}", "{".repeat(200_000), "}".repeat(200_000)),
        format!("cat {{{}}}", "a,".repeat(500_000)),
        format!("cat x{{{}}}", "{a,b}".repeat(100_000)),
        format!("cat {}", "{a,{b,{c,".repeat(50_000)),
    ];
    let started = std::time::Instant::now();
    let checked = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            for input in &inputs {
                bash_refused(input, &[]);
                words::unquote_words(input);
            }
        })
        .unwrap()
        .join();
    assert!(checked.is_ok());
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(!bash_allowed(
        &format!("cat {}", "{a,b}".repeat(20_000)),
        &[]
    ));
    assert!(bash_allowed("ls {a,b}{c,d}", &[]));
}

#[test]
fn brace_expansion_matches_bash() {
    let words = |segment: &str| expanded(segment).unwrap();
    assert_eq!(words("{a,b}{c,d}"), ["ac", "ad", "bc", "bd"]);
    assert_eq!(words("x{a,{b,c}}y"), ["xay", "xby", "xcy"]);
    assert_eq!(words("{{a,b}"), ["{a", "{b"]);
    assert_eq!(words("{a,b}}"), ["a}", "b}"]);
    assert_eq!(words("{a}b,c}"), ["{a}b,c}"]);
    assert_eq!(words("{3..1} {a..c..2}"), ["3", "2", "1", "a", "c"]);
    assert_eq!(words("{1..9..4} {-1..1}"), ["1", "5", "9", "-1", "0", "1"]);
    assert_eq!(
        words("{x..} {1..a} {\"1\"..2}"),
        ["{x..}", "{1..a}", "{1..2}"]
    );
    assert_eq!(words("{1..2..-9223372036854775808}"), ["1"]);
    for refused in [
        "{-9223372036854775807..9223372036854775807}",
        "{9223372036854775807..-9223372036854775808}",
        "{1..99999999999999999999}",
        "{1..2..99999999999999999999}",
    ] {
        assert!(expanded(refused).is_none(), "{refused}");
    }
}

#[test]
fn bypasses_through_read_only_names_are_refused() {
    for command in [
        "rg --hostname-bin=/tmp/x --hyperlink-format=default foo",
        "rg --hostname-bin /tmp/x foo",
        "rg --hostname=/tmp/x foo",
        "git branch --unset-u",
        "git branch --set-up=origin/main",
        "git branch --edit",
        "git branch --del old",
        "git branch --mo old new",
        "cat x >2>&1/dev/null",
        "echo hi >/dev/nullx",
        "echo hi 12>&1",
        "find -- . *",
        "cargo clippy -- *",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "rg --hidden foo",
        "git branch --cont HEAD",
        "git branch --sort=-committerdate",
        "git branch --format '%(refname)'",
        "cargo check 2>&1|tail -3",
        "ls >/dev/null 2>&1",
        "ls &>/dev/null",
        "rg foo -- *",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn ordinary_exploration_still_runs() {
    for command in [
        "grep -E 'a|b' src",
        "rg 'foo bar' src",
        "find . -name '*.rs' | xargs wc -l",
        "sed -n '1,20p' f",
        "git log --format='%h %s'",
        "ls # list",
        "echo $HOME",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn bash_and_the_split_read_the_same_text() {
    for command in [
        "ls $$'\\';rm -rf victim;echo ''",
        "echo $$'\\' ; touch P1 ; echo ''",
        "ls \\>/dev/null;rm -rf victim",
        "ls \\>/dev/null|rm -rf victim",
        "ls \\>/dev/null\nrm -rf victim",
        "ls \\2>&1;rm -rf victim",
        "ls \\\n#'\ntouch PWNED\n#'",
        "ls \\\n#<<EOF\ntouch PWNED\nEOF",
        "ls ?(#x); touch P",
        "ls ?(a)#b; touch P",
    ] {
        assert!(!bash_allowed(command, &[]), "{command:?}");
    }
    assert_eq!(
        bash_refused("ls \\\n#'\ntouch PWNED\n#'", &[]).as_deref(),
        Some("touch PWNED")
    );
    for segment in ["a;b", "a|b", "a&b", "a\nb", "a(b", "a)b", "a # x\nb"] {
        assert!(expanded(segment).is_none(), "{segment:?}");
    }
    for segment in ["'a;b'", "a\\;b", "\"a|b\"", "a\\\nb"] {
        assert!(expanded(segment).is_some(), "{segment:?}");
    }
}

#[test]
fn only_redirects_to_nowhere_pass() {
    for command in [
        "ls 1>/dev/null",
        "ls 2> /dev/null",
        "ls >>/dev/null",
        "ls >&2",
        "ls 2>&1 | head",
        "ls &>/dev/null",
        "ls>/dev/null",
        "cat <&0",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "ls >& out",
        "ls &> out",
        "ls 2>&1x",
        "ls 3>/dev/null",
        "ls >| /dev/null",
        "ls > '/dev/null'x",
        "find . <2>/dev/null -delete",
        "find . < >/dev/null -delete",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    assert_eq!(
        expanded("find . <2>/dev/null -name x").unwrap(),
        ["find", ".", "-name", "x"]
    );
}

#[test]
fn operands_whose_place_matters_must_be_seen() {
    for command in [
        "uniq Cargo.[lt]o*",
        "uniq a?",
        "uniq -- *",
        "uniq src/*",
        "uniq $X",
        "uniq -- -a -b",
        "sed -n -- $TERM f",
        "sed -n -- $p f",
        "sed -n /x*/p f",
        "sed -n -e $X f",
        "sed -n --expression $X f",
        "xxd f out",
        "xxd -r dump out",
        "xxd -revert dump",
        "xxd -c 8 f out",
        "xxd f*",
        "xxd -- -r out",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "uniq -c f",
        "uniq -f 1 -- f",
        "sed -n p src/*.rs",
        "sed -n -e p -- src/*.rs",
        "xxd f",
        "xxd -l 64 -c 16 f",
        "head -c 200 f | xxd",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn build_tools_refuse_what_names_an_output_or_a_program() {
    for command in [
        "cargo clippy -- --emit=metadata=/tmp/x",
        "cargo clippy -- -D warnings --emit=metadata=/tmp/x",
        "cargo clippy -- -D --emit=metadata=/tmp/x",
        "cargo clippy -- -Zunpretty=x",
        "cargo check --target-dir /tmp/x",
        "cargo check --target-dir=/tmp/x",
        "cargo fmt",
        "cargo fmt --check -- --print-config default rustfmt.toml",
        "go list -export -gcflags=-o=/tmp/x ./...",
        "go list -asmflags=x ./...",
        "go list --ldflags x ./...",
        "go list -gccgoflags=x ./...",
        "go list -pkgdir /tmp/x ./...",
        "go list -modfile=x.mod ./...",
        "go list -mod=mod ./...",
        "go vet -mod mod ./...",
        "pnpm ls --config.pnpmfile=x.cjs",
        "pnpm ls --pnpmfile x.cjs",
        "pnpm ls --pnpmf=x.cjs",
        "pnpm ls --global-pnpmfile=x.cjs",
        "npm ls --logs-dir=/tmp/x",
        "npm ls --logs-dir /tmp/x",
        "npm view x --cache /tmp/y",
        "xcodebuild -list -derivedDataPath /tmp/x",
        "xcodebuild -list build",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "cargo clippy -- -D warnings",
        "cargo clippy --all-targets -- -Dwarnings -A clippy::x --allow=y",
        "cargo check --target wasm32-unknown-unknown",
        "cargo fmt --check",
        "cargo fmt --check -p killi",
        "go list -m all",
        "go vet ./...",
        "pnpm ls --global",
        "pnpm ls --depth 0",
        "npm ls --long",
        "xcodebuild -list",
        "xcodebuild -list -json -project App.xcodeproj",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn harmless_lookups_pass() {
    for command in [
        "git stash list",
        "git stash show -p stash@{0}",
        "git reflog -10",
        "git reflog show main",
        "git show-ref --tags",
        "true",
        "rg foo || true",
        "false",
        "test -f Cargo.toml && echo yes",
        "[ -d target ] && echo built",
        "printenv PATH",
        "env",
        "env | grep RUST",
        "od -c file | head",
    ] {
        assert!(bash_allowed(command, &[]), "{command}");
    }
    for command in [
        "git reflog expire --all",
        "git reflog delete HEAD@{1}",
        "git stash drop",
        "env rm -rf x",
        "env FOO=1 ls",
        "env -i sh",
    ] {
        assert!(!bash_allowed(command, &[]), "{command}");
    }
}

#[test]
fn one_brace_budget_covers_the_whole_command() {
    let inputs = [
        "cat {1..1023}{1..1};".repeat(10_000),
        "ls {1..1023}{1..1} ".repeat(10_000),
        "git status {a,b}{c,d}{e,f};".repeat(8_000),
    ];
    let started = std::time::Instant::now();
    for input in &inputs {
        assert!(!bash_allowed(input, &[]));
    }
    // About 15 ms each in a release build; the per-segment budget took seconds.
    let limit = if cfg!(debug_assertions) { 3000 } else { 300 };
    assert!(
        started.elapsed() < std::time::Duration::from_millis(limit),
        "{:?}",
        started.elapsed()
    );
}
