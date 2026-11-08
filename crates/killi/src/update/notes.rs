//! The changelog's sections for the releases a session would update across.

use super::parse;

/// One release's part of `CHANGELOG.md`: its heading (`0.6.63 — 2026-11-08`) and its notes,
/// each list item on one line so a narrow screen can wrap it again.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub heading: String,
    pub body: String,
}

/// The sections of `changelog` for versions after `current` up to `latest`, newest first as
/// the file has them. `## Unreleased` and anything without a version are left out.
pub fn between(changelog: &str, current: &str, latest: &str) -> Vec<Section> {
    let (current, latest) = (parse(current), parse(latest));
    let mut sections: Vec<Section> = Vec::new();
    let mut keep = false;
    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let version = heading.split_whitespace().next().unwrap_or_default();
            let numeric = version.starts_with(|c: char| c.is_ascii_digit());
            let at = parse(version);
            keep = numeric && at > current && at <= latest;
            if keep {
                sections.push(Section {
                    heading: heading.trim().to_string(),
                    body: String::new(),
                });
            }
            continue;
        }
        if !keep {
            continue;
        }
        let body = &mut sections.last_mut().expect("a kept section").body;
        let continues = line.starts_with(' ') && !line.trim().is_empty();
        if continues && !body.is_empty() && !body.ends_with('\n') {
            body.push(' ');
            body.push_str(line.trim());
        } else {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(line.trim_end());
        }
    }
    for section in &mut sections {
        section.body = section.body.trim().to_string();
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = "# Changelog\n\nIntro.\n\n## Unreleased\n\n- not out yet\n\n\
        ## 0.6.64 — 2026-11-09\n\n- One item that wraps\n  onto a second line.\n- Two.\n\n\
        ## 0.6.63 — 2026-11-08\n\n- Three.\n\n## 0.6.62 — 2026-11-08\n\n- Four.\n";

    #[test]
    fn only_the_releases_after_the_current_one_up_to_the_latest() {
        let sections = between(CHANGELOG, "0.6.62", "0.6.64");
        assert_eq!(
            sections,
            vec![
                Section {
                    heading: "0.6.64 — 2026-11-09".into(),
                    body: "- One item that wraps onto a second line.\n- Two.".into(),
                },
                Section {
                    heading: "0.6.63 — 2026-11-08".into(),
                    body: "- Three.".into(),
                },
            ]
        );
        assert_eq!(between(CHANGELOG, "0.6.63", "0.6.63"), vec![]);
        assert_eq!(between(CHANGELOG, "0.6.62", "0.6.63").len(), 1);
    }

    #[test]
    fn a_changelog_without_versions_gives_nothing() {
        assert_eq!(between("<html>not found</html>", "0.1.0", "0.2.0"), vec![]);
    }
}
