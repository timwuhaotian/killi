//! `/advisor`: the stronger model every session's model can ask through its `advisor` tool,
//! saved in the config as `advisor_model`, and the advice drawn under the call.

use super::*;

/// Rows of advice drawn under an `advisor` call; Ctrl+O shows the rest.
const ADVICE_ROWS: usize = 40;

impl App {
    /// Applies `/advisor <alias>` or `/advisor off`: checked before it is saved, so the config
    /// never names an advisor it can't run, then in force here when this session has the tool
    /// and in every session started after it.
    pub(super) fn advisor_pick(&mut self, agent: &mut Agent, session: &mut Setup, alias: &str) {
        let path = crate::config::killi_home().join("config.toml");
        if alias.is_empty() || matches!(alias, "none" | "off" | "clear") {
            if let Err(e) = crate::config::set_advisor_model(&path, None) {
                return self.error_from("/advisor", &e);
            }
            agent.clear_advisor();
            session.set_advisor_model(None);
            return self.notice("No advisor, here or in new sessions.");
        }
        if !session.config().model_allowed(alias) {
            return self.notice(&format!(
                "`{alias}` is hidden by [models_filter.<provider>]."
            ));
        }
        let client = match session.config().resolve(Some(alias)).and_then(Client::new) {
            Ok(client) => client,
            Err(e) => return self.error_report(&e),
        };
        if let Err(e) = crate::config::set_advisor_model(&path, Some(alias)) {
            return self.error_from("/advisor", &e);
        }
        session.set_advisor_model(Some(alias));
        let same = if alias == agent.model_alias() {
            " It is also this session's model; a stronger one gives better advice.".to_string()
        } else {
            String::new()
        };
        if agent.has_advisor_tool() {
            agent.set_advisor(client);
            self.notice(&format!(
                "Advisor {alias}, here and in every new session: {} can ask it for advice when it needs a second opinion.{same}",
                agent.model_alias()
            ));
        } else {
            self.notice(&format!(
                "Advisor {alias} for every new session. This one started without the advisor tool and its tools can't change mid-session, so /new starts one that has it.{same}"
            ));
        }
    }

    /// Draws an `advisor` call's advice under its line: its Markdown styled as the model's
    /// replies are, behind a bar that sets it apart from them, never as a diff.
    pub(super) fn advice_rows(&mut self, advice: &str) {
        let lines: Vec<&str> = advice.lines().collect();
        let shown = lines.len().min(ADVICE_ROWS);
        let mut markdown = Markdown::default();
        let mut parts = Vec::new();
        for line in &lines[..shown] {
            let line = text::sanitize_row(line);
            if line.trim().is_empty() {
                parts.extend(markdown.finish_parts());
                parts.push(Part::Row(String::new()));
            } else {
                parts.extend(markdown.parts(&line));
            }
        }
        parts.extend(markdown.finish_parts());
        let bar = format!("  {FAINT}│{RESET} ");
        for part in parts {
            match part {
                Part::Row(row) => {
                    let indent = 4 + text::hanging_indent(&row);
                    self.emit(Line::Words(format!("{bar}{row}"), indent));
                }
                Part::Code(row) => {
                    let row = format!("{bar}{row}");
                    self.emit(Line::block(move |_| vec![row.clone()]));
                }
                Part::Table(rows) => {
                    let bar = bar.clone();
                    self.emit(Line::block(move |width| {
                        text::render_table(&rows, width.saturating_sub(4))
                            .into_iter()
                            .map(|row| format!("{bar}{row}"))
                            .collect()
                    }));
                }
            }
        }
        self.hidden_note(lines.len() - shown, "more");
    }
}
