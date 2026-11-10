//! A session's card: what it is, what it can be asked to do and what it is doing now.
//! `killi card <pid>` prints it — as a few lines, or as JSON with `--json` — so the user, a
//! script or another session can tell what a session is for before asking it anything. Like
//! the rest of the live sessions it reaches no further than this machine's own sockets, and
//! it says only what the session answers: `ask` and `tell`, and tasks (`killi send`) when the
//! session's own status says it takes them.

use serde_json::{Value, json};

use super::{Entry, describe, tilde};

/// The request shapes a session speaks on its socket: `killi-live/1` is status, ask and tell,
/// and `killi-live/2` adds tasks and their event streams. A session's file carries no protocol
/// of its own, so the card reads it off the session's status.
fn protocol(tasks: bool) -> &'static str {
    if tasks {
        "killi-live/2"
    } else {
        "killi-live/1"
    }
}

/// Whether the session takes tasks: its status counts the ones it runs, which a release from
/// before tasks never does, and a headless session answers no status at all. So a card never
/// promises a task to a session that would answer `killi send` with an error.
fn takes_tasks(status: Option<&Value>) -> bool {
    status.is_some_and(|status| status["tasks"].is_u64())
}

/// What a session can be asked to do, as its card says it. Fixed text: nothing derives these
/// from the session, so a card never promises more than the session can answer.
const SKILLS: &[(&str, &str, &str)] = &[
    (
        "code",
        "Change code",
        "read, change and run the code in this repository",
    ),
    (
        "checks",
        "Run checks",
        "run this project's builds, tests and commands",
    ),
    (
        "work",
        "Report on its work",
        "say what it has done, which files it changed and what is left",
    ),
];

/// The card of `entry` as JSON, with what it is doing now from `status` when it answered.
pub fn card(entry: &Entry, status: Option<&Value>) -> Value {
    let answers = entry.socket.is_some();
    let tasks = answers && takes_tasks(status);
    let mut card = json!({
        "name": format!("killi {}", entry.pid),
        "description": about(entry),
        "version": entry.version,
        "protocol": protocol(tasks),
        "state": if entry.working { "working" } else { "idle" },
        "model": entry.model,
        "cwd": entry.cwd,
        "tree": entry.tree,
        "branch": entry.branch,
        "capabilities": {
            "ask": answers,
            "tell": answers,
            "tasks": tasks,
            // A task's steps as they happen, with `killi task <id> --follow`.
            "streaming": tasks,
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/markdown"],
        "skills": skills(),
    });
    if let Some(socket) = &entry.socket {
        card["endpoint"] = json!(socket);
    }
    if status.and_then(|status| status["plan"].as_bool()) == Some(true) {
        card["plan"] = json!(true);
    }
    card
}

/// The card of `entry` as a few lines for a terminal.
pub fn card_text(entry: &Entry, status: Option<&Value>, now: u64) -> String {
    let tasks = entry.socket.is_some() && takes_tasks(status);
    let mut text = format!(
        "{} · killi {} · {}",
        describe(entry, now),
        entry.version,
        protocol(tasks)
    );
    let others = if tasks {
        "ask it about its work (`killi ask <pid> \"…\"`), leave it a message (`killi tell <pid> \"…\"`), hand it a task (`killi send <pid> \"…\"`)"
    } else if entry.socket.is_some() {
        "ask it about its work (`killi ask <pid> \"…\"`), leave it a message (`killi tell <pid> \"…\"`)"
    } else {
        "nothing: it runs headless (-p) and takes no questions, messages or tasks"
    };
    text.push_str(&format!("\nOthers can: {others}."));
    text.push_str(&format!("\nSkills: {}", skills_text()));
    if status.and_then(|status| status["plan"].as_bool()) == Some(true) {
        text.push_str("\nIn plan mode.");
    }
    text
}

/// What the session is working on, in one line: its prompt, or where it sits.
fn about(entry: &Entry) -> String {
    let prompt = entry.prompt.trim();
    if prompt.is_empty() {
        return format!("A killi session in {}", tilde(&entry.cwd));
    }
    crate::llm::truncate(prompt, 200).to_string()
}

fn skills() -> Value {
    json!(SKILLS
        .iter()
        .map(|(id, name, description)| json!({"id": id, "name": name, "description": description}))
        .collect::<Vec<_>>())
}

fn skills_text() -> String {
    SKILLS
        .iter()
        .map(|(_, name, description)| format!("{name} — {description}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32) -> Entry {
        Entry {
            pid,
            version: "0.6.71".into(),
            cwd: "/w".into(),
            tree: "/w".into(),
            prompt: "fix slugify".into(),
            ..Entry::default()
        }
    }

    #[test]
    fn the_card_says_what_a_session_is_and_what_it_can_be_asked_to_do() {
        let mut live = entry(41);
        live.socket = Some("/tmp/41.sock".into());
        // The status of a release from before tasks: it counts none.
        let old = json!({"ok": true, "working": false});
        let shown = card(&live, Some(&old));
        assert_eq!(shown["name"], "killi 41");
        assert_eq!(shown["description"], "fix slugify");
        assert_eq!(shown["version"], "0.6.71");
        assert_eq!(shown["protocol"], "killi-live/1");
        assert_eq!(shown["state"], "idle");
        assert_eq!(shown["capabilities"]["ask"], true);
        assert_eq!(shown["capabilities"]["tasks"], false);
        assert_eq!(shown["defaultOutputModes"], json!(["text/markdown"]));
        assert_eq!(shown["endpoint"], "/tmp/41.sock");
        assert_eq!(shown["skills"].as_array().map(Vec::len), Some(3));
        assert_eq!(shown["skills"][0]["id"], "code");
        assert!(shown.get("plan").is_none());

        let text = card_text(&live, Some(&old), 1_000);
        assert!(
            text.starts_with(
                "41 · idle for 1s · /w · “fix slugify” · killi 0.6.71 · killi-live/1"
            ),
            "{text}"
        );
        assert!(text.contains("Others can: ask it about its work"), "{text}");
        assert!(!text.contains("killi send"), "{text}");
        assert!(text.contains("Skills: Change code —"), "{text}");
        assert!(!text.contains("plan mode"));
    }

    #[test]
    fn a_session_whose_status_counts_its_tasks_says_it_takes_them() {
        let mut live = entry(45);
        live.socket = Some("/tmp/45.sock".into());
        let status = json!({"ok": true, "working": false, "tasks": 0});
        let shown = card(&live, Some(&status));
        assert_eq!(shown["protocol"], "killi-live/2");
        assert_eq!(shown["capabilities"]["tasks"], true);
        assert_eq!(shown["capabilities"]["streaming"], true);
        let text = card_text(&live, Some(&status), 0);
        assert!(text.contains("· killi-live/2"), "{text}");
        assert!(
            text.contains("hand it a task (`killi send <pid> \"…\"`)"),
            "{text}"
        );
    }

    #[test]
    fn a_headless_session_says_so_and_an_idle_one_names_its_folder() {
        let headless = card(&entry(42), None);
        assert_eq!(headless["capabilities"]["ask"], false);
        assert_eq!(headless["capabilities"]["tasks"], false);
        assert!(headless.get("endpoint").is_none());
        assert!(card_text(&entry(42), None, 0).contains("runs headless (-p)"));

        let mut empty = entry(43);
        empty.prompt = "  ".into();
        assert_eq!(card(&empty, None)["description"], "A killi session in /w");
    }

    #[test]
    fn plan_mode_reaches_the_card_from_its_status() {
        let mut live = entry(44);
        live.working = true;
        let status = json!({"plan": true, "todos": []});
        assert_eq!(card(&live, Some(&status))["plan"], true);
        assert_eq!(card(&live, Some(&status))["state"], "working");
        assert!(card_text(&live, Some(&status), 0).ends_with("In plan mode."));
    }
}
