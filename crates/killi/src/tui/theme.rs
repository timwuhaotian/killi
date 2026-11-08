use super::*;

/// Height from which the specimen shows the block-letter logo with its side lines, as the
/// banner itself does; below it the wordmark stands in, so the sample rows keep their room.
const LOGO_ROWS: usize = 34;
/// Height from which every sample row shows; below it the working row goes, which the
/// picker's own selection band already stands in for.
const FULL_ROWS: usize = 28;

impl App {
    /// What the theme being previewed paints, while the /theme list is open: the header as
    /// the banner draws it, a swatch for every color's job, then one of each element the
    /// colors sit on — your message's band, finished and failed calls, a diff, prose and
    /// code, a warning and the task marks — each built by the builders the conversation
    /// itself uses, so the sample is the thing it samples. Drawn fresh on every frame in
    /// the previewed theme, like the rest of the live area.
    pub(super) fn theme_specimen_rows(&self, width: usize) -> Vec<String> {
        let name = text::skin_name();
        let version = env!("CARGO_PKG_VERSION");
        let mut rows = vec![title_rule(&format!("Theme preview · {name}"), width)];
        if self.height >= LOGO_ROWS && !banner::compact(width, true) {
            let room = width.saturating_sub(41).max(20);
            let side = vec![
                format!("{BOLD}killi{RESET} {DIM}{version}{RESET}"),
                format!("{DIM}a coding agent for the terminal{RESET}"),
                String::new(),
                format!(
                    "{}{BOLD}{}{RESET}",
                    label("model"),
                    if self.status.model.is_empty() {
                        "model"
                    } else {
                        &self.status.model
                    }
                ),
                format!(
                    "{}{}",
                    label("folder"),
                    shorten_path(&tilde(&self.cwd), room)
                ),
            ];
            rows.extend(banner::render(width, &side));
        } else {
            rows.push(fit(
                &format!("{} {DIM}· {name}{RESET}", banner::wordmark(version)),
                width,
            ));
        }
        let swatch = |ink: text::Sgr, what: &str| format!("{ink}██{RESET} {DIM}{what}{RESET}");
        let ground = |ink: text::Sgr, what: &str| format!("{ink}  {RESET} {DIM}{what}{RESET}");
        rows.extend(wrap_items(
            &[
                swatch(LAGOON, "work"),
                swatch(FIN, "you"),
                swatch(WARN, "warning"),
                swatch(DANGER, "error"),
                swatch(EFFORT, "effort"),
                swatch(TOKENS, "tokens"),
                swatch(BRANCH, "branch"),
                swatch(DIM, "secondary"),
                swatch(FAINT, "faint"),
                ground(TINT, "your band"),
                ground(SELECT, "selected"),
                ground(ADD, "added"),
                ground(DEL, "removed"),
                ground(CHIP, "code"),
            ],
            width,
        ));
        rows.extend(prompt_rows("make the failing test pass", width));
        rows.push(fit(
            &format!(
                "{LAGOON}✓{RESET} {} {DIM}· 12ms{RESET}",
                tool_summary("read src/main.rs", false)
            ),
            width,
        ));
        rows.push(fit(
            &format!(
                "{BOLD}{DANGER}✗{RESET} {} {DANGER}1 failed{RESET}",
                tool_summary(&tool_text("bash cargo test"), false)
            ),
            width,
        ));
        if self.height >= FULL_ROWS {
            // A settled frame, not the live spinner's: the word shows whole, as it settles.
            rows.extend(
                working_rows(
                    40,
                    Duration::from_secs(3),
                    "working",
                    "",
                    "1.2k out",
                    width,
                )
                .into_iter()
                .map(|row| fit(&row, width)),
            );
        }
        rows.extend(diff_rows(
            &[
                "+7\tlet theme = \"previewed\";".to_string(),
                "-7\tlet theme = \"before\";".to_string(),
            ],
            width,
            text::Lang::of_path("specimen.rs"),
        ));
        let mut markdown = Markdown::new(width);
        let mut styled = markdown.line("Set `theme` in **config**, then *restart*:");
        styled.extend(markdown.line("```rust"));
        styled.extend(markdown.line("let count = 42; // \"snow\""));
        styled.extend(markdown.finish());
        rows.extend(styled.iter().flat_map(|row| wrap(row, width)));
        rows.extend(wrap_words(
            &format!(
                "{WARN}!{RESET} {DIM}retrying in 2s{RESET}   {DANGER}✗ Error:{RESET} a sample failure   {BOLD}{FIN}?{RESET} {BOLD}keep this theme?{RESET}"
            ),
            width,
            2,
        ));
        rows.extend(wrap_words(
            &format!(
                "  {LAGOON}✓{RESET} {DIM}done{RESET}  {LAGOON}◐{RESET} doing  {FAINT}○{RESET} {DIM}to do{RESET}   {BRANCH}⎇ main{RESET}{WARN}*{RESET}"
            ),
            width,
            2,
        ));
        rows.iter().map(|row| fit(row, width)).collect()
    }
}
