//! Swarms: one `agent_swarm` call runs a subagent per item of a prompt template, and swarm
//! mode (`/swarm`, on unless `[agent] swarm = false`) tells the agent to split work that way.
//! Like plan mode, the mode reaches the model as appended reminders, so the system prompt and
//! tool list stay as they are.

use std::collections::HashMap;

use serde_json::Value;

use crate::tools::{self, Output};

pub const ITEM: &str = "{{item}}";
pub const MAX_ITEMS: usize = 128;

pub const ON: &str = "<system-reminder>Swarm mode is on. When a task splits into independent parts (several files, PRs, modules, questions), explore only as much as you need to split it, then hand the parts to agent_swarm: a prompt_template with {{item}} and one item per part, with no two subagents touching the same thing. Give each only the background it needs, and don't hold back on the number of subagents (up to 128) unless the user sets a limit. A task with one part, or a small one, you do yourself.</system-reminder>";

pub const OFF: &str = "<system-reminder>Swarm mode is off. Use agent_swarm only when a request clearly benefits from it.</system-reminder>";

/// Each item with its filled-in prompt, or why the call can't run.
pub fn prompts(args: &Value) -> Result<Vec<(String, String)>, String> {
    let template = args["prompt_template"].as_str().unwrap_or_default().trim();
    if !template.contains(ITEM) {
        return Err(format!("prompt_template must contain {ITEM}."));
    }
    let items: Vec<String> = args["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
        .filter(|item| !item.is_empty())
        .collect();
    if items.len() < 2 {
        return Err("agent_swarm needs at least 2 items; for one task, use agent.".into());
    }
    if items.len() > MAX_ITEMS {
        return Err(format!(
            "agent_swarm takes at most {MAX_ITEMS} items; you sent {}.",
            items.len()
        ));
    }
    let mut seen = HashMap::new();
    let mut prompts = Vec::new();
    for (n, item) in items.into_iter().enumerate() {
        let prompt = template.replace(ITEM, &item);
        if let Some(first) = seen.insert(prompt.clone(), n + 1) {
            return Err(format!(
                "Items {first} and {} give the same prompt; each subagent needs distinct work.",
                n + 1
            ));
        }
        prompts.push((item, prompt));
    }
    Ok(prompts)
}

/// Every subagent's report in item order. When they don't fit in `limit` characters, each is
/// cut to an equal share and the full text is saved to a file.
pub fn report(ctx: &tools::Context, items: &[String], outputs: &[Output], limit: usize) -> Output {
    let failed = outputs.iter().filter(|o| o.is_error).count();
    let done = outputs.len() - failed;
    let summary = if failed == 0 {
        format!("completed: {done}")
    } else {
        format!("completed: {done}, failed: {failed}")
    };
    let render = |share: Option<usize>, note: &str| {
        let mut text = format!("<agent_swarm_result>\n<summary>{summary}</summary>\n");
        for (item, output) in items.iter().zip(outputs) {
            let outcome = if output.is_error {
                "failed"
            } else {
                "completed"
            };
            let body = match share {
                Some(share) if output.text.len() > share => {
                    format!("{} […]", crate::llm::truncate(&output.text, share))
                }
                _ => output.text.clone(),
            };
            text.push_str(&format!(
                "<subagent item=\"{}\" outcome=\"{outcome}\">\n{body}\n</subagent>\n",
                escape(item)
            ));
        }
        text.push_str(note);
        text.push_str("</agent_swarm_result>");
        text
    };
    let retry = if failed > 0 {
        "Run the failed items again with agent_swarm or agent if their work is still needed.\n"
    } else {
        ""
    };
    let mut text = render(None, retry);
    if text.len() > limit {
        let saved = match tools::spill(ctx, "swarm", &text) {
            Some(path) => format!(
                "Reports marked […] were cut to fit; all of them are in {}.\n",
                path.display()
            ),
            None => "Reports marked […] were cut to fit.\n".into(),
        };
        let overhead: usize = items.iter().map(|item| item.len() + 64).sum::<usize>()
            + saved.len()
            + retry.len()
            + 128;
        let share = (limit * 9 / 10).saturating_sub(overhead) / items.len().max(1);
        text = render(Some(share.max(200)), &format!("{retry}{saved}"));
    }
    let output = if done == 0 {
        Output::error(text)
    } else {
        Output::ok(text)
    };
    output.brief(if failed == 0 {
        format!("{done} done")
    } else {
        format!("{done} done, {failed} failed")
    })
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn prompts_fill_the_template_and_reject_bad_calls() {
        let args = json!({"prompt_template": "Review {{item}}.", "items": ["a.rs", " b.rs "]});
        assert_eq!(
            prompts(&args).unwrap(),
            vec![
                ("a.rs".into(), "Review a.rs.".into()),
                ("b.rs".into(), "Review b.rs.".into())
            ]
        );
        assert!(
            prompts(&json!({"prompt_template": "Review.", "items": ["a", "b"]}))
                .unwrap_err()
                .contains("{{item}}")
        );
        assert!(
            prompts(&json!({"prompt_template": "{{item}}", "items": ["a"]}))
                .unwrap_err()
                .contains("at least 2")
        );
        assert!(
            prompts(&json!({"prompt_template": "{{item}}", "items": ["a", "a"]}))
                .unwrap_err()
                .contains("Items 1 and 2")
        );
        let many: Vec<String> = (0..=MAX_ITEMS).map(|n| n.to_string()).collect();
        assert!(
            prompts(&json!({"prompt_template": "{{item}}", "items": many}))
                .unwrap_err()
                .contains("at most")
        );
    }

    #[test]
    fn report_lists_every_item_and_cuts_long_reports_to_fit() {
        let dir = std::env::temp_dir().join(format!("killi-swarm-{}", std::process::id()));
        let ctx = tools::Context::new(dir.clone(), dir.clone());
        let items = vec!["a\"1".to_string(), "b".to_string()];
        let outputs = vec![Output::ok("fine"), Output::error("broke")];
        let short = report(&ctx, &items, &outputs, 10_000);
        assert!(!short.is_error);
        assert!(
            short
                .text
                .contains("<summary>completed: 1, failed: 1</summary>")
        );
        assert!(
            short
                .text
                .contains("<subagent item=\"a&quot;1\" outcome=\"completed\">\nfine\n</subagent>")
        );
        assert!(short.text.contains("Run the failed items again"));

        let outputs = vec![
            Output::ok("x".repeat(30_000)),
            Output::ok("y".repeat(30_000)),
        ];
        let long = report(&ctx, &items, &outputs, 20_000);
        assert!(long.text.len() <= 20_000, "{}", long.text.len());
        assert!(long.text.contains("[…]") && long.text.contains("all of them are in"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
