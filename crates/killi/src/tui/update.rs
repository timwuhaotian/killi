//! The offer to update. A session asks the release site for a newer killi when it starts and
//! every few hours after; when there is one, it prints what changed since this version and
//! asks at once whether to install it. The install runs beside the session, and the new
//! binary is used from the next start. `KILLI_NO_UPDATE_CHECK` turns the checks off.

use super::*;
use crate::update::{self, Release, notes::Section};
use picker::PickerChoice;

/// How long after one check the next one starts.
const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);
/// The most releases whose notes are printed; older ones are only counted.
const MAX_SECTIONS: usize = 3;

/// The checks, the release waiting to be offered, and the install under way. Tasks are
/// spawned, so a check or a download keeps going while a turn runs; its result is taken at
/// the next prompt.
#[derive(Default)]
pub(super) struct Updates {
    /// When the next check starts; never when checks are off or this session installed one.
    pub(super) due: Option<Instant>,
    pub(super) check: Option<tokio::task::JoinHandle<Result<Option<Release>>>>,
    /// A newer release, waiting for the prompt to be free to ask about it.
    pub(super) found: Option<Release>,
    /// The release the open picker asks about.
    pub(super) asking: Option<Release>,
    /// The version the user said no to: not asked about again in this session.
    pub(super) declined: Option<String>,
    /// The version being installed, and its install.
    pub(super) install: Option<(String, tokio::task::JoinHandle<Result<()>>)>,
    /// A newer killi is installed: nothing more to check or offer in this session.
    pub(super) installed: bool,
}

impl Updates {
    /// Checks due at once, unless `KILLI_NO_UPDATE_CHECK` is set.
    pub(super) fn new() -> Self {
        let off = std::env::var_os("KILLI_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty());
        Self {
            due: (!off).then(Instant::now),
            ..Self::default()
        }
    }
}

impl App {
    pub(super) fn start_update_check(&mut self) {
        self.updates.due = None;
        self.updates.check = Some(tokio::spawn(update::check()));
    }

    /// Takes a check's answer: a newer version not yet declined is offered as soon as the
    /// prompt is free. A failed check says nothing; the next one is a few hours away.
    pub(super) fn on_update_checked(
        &mut self,
        result: std::result::Result<Result<Option<Release>>, tokio::task::JoinError>,
    ) {
        self.updates.check = None;
        if self.updates.installed {
            return;
        }
        self.updates.due = Some(Instant::now() + RECHECK);
        if let Ok(Ok(Some(release))) = result
            && self.updates.declined.as_deref() != Some(&release.version)
            && self.updates.asking.is_none()
            && self.updates.install.is_none()
        {
            self.updates.found = Some(release);
        }
        self.offer_update();
    }

    /// Prints the release's notes and opens the picker that asks whether to install it, when
    /// a release waits and nothing else has the keys: no picker or question open, nothing
    /// typed, no turn running.
    pub(super) fn offer_update(&mut self) {
        if self.picker.is_some()
            || self.asking.is_some()
            || !self.editor.is_empty()
            || self.busy.is_some()
        {
            return;
        }
        let Some(release) = self.updates.found.take() else {
            return;
        };
        let (version, notes) = (release.version.clone(), release.notes.clone());
        self.begin(Kind::Note);
        self.emit(Line::block(move |width| release_rows(&version, &notes, width)));
        self.commit(String::new());
        let current = env!("CARGO_PKG_VERSION");
        self.picker = Some(Picker::new(
            format!("Update killi {current} → {}?", release.version),
            vec![
                (
                    "Update now".into(),
                    "download and install it; restart killi to use it".into(),
                    PickerChoice::Update(true),
                ),
                (
                    "Not now".into(),
                    "ask again next session".into(),
                    PickerChoice::Update(false),
                ),
            ],
            0,
        ));
        self.updates.asking = Some(release);
    }

    /// The update picker's answer; `install` false is the same as closing it.
    pub(super) fn on_update_choice(&mut self, install: bool) {
        let Some(release) = self.updates.asking.take() else {
            return;
        };
        if !install {
            self.notice(&format!(
                "Staying on killi {}; `killi update` installs {} any time.",
                env!("CARGO_PKG_VERSION"),
                release.version
            ));
            self.updates.declined = Some(release.version);
            return;
        }
        self.notice(&format!("Installing killi {}…", release.version));
        let version = release.version.clone();
        self.updates.install = Some((version, tokio::spawn(update::install(release))));
    }

    /// The update picker closed without an answer (Esc, Ctrl+C): taken as "not now".
    pub(super) fn update_dismissed(&mut self) {
        if self.updates.asking.is_some() {
            self.on_update_choice(false);
        }
    }

    pub(super) fn on_update_installed(
        &mut self,
        result: std::result::Result<Result<()>, tokio::task::JoinError>,
    ) {
        let Some((version, _)) = self.updates.install.take() else {
            return;
        };
        match result.map_err(anyhow::Error::from).and_then(|r| r) {
            Ok(()) => {
                self.updates.due = None;
                self.updates.installed = true;
                self.begin(Kind::Note);
                self.commit(format!(
                    "{LAGOON}✓{RESET} Installed killi {version}. This session stays on {}; /exit and `killi -c` continue it on {version}.",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            Err(e) => {
                self.error_from(&format!("could not install killi {version}"), &e);
                self.notice("`killi update` in a shell tries again.");
            }
        }
    }
}

/// What a newer release changes: a head naming it, then each release's heading and notes,
/// newest first, the oldest past `MAX_SECTIONS` only counted.
pub(super) fn release_rows(version: &str, notes: &[Section], width: usize) -> Vec<String> {
    let current = env!("CARGO_PKG_VERSION");
    let mut rows = wrap_words(
        &format!(
            "{BOLD}{LAGOON}killi {version}{RESET}{BOLD} is out{RESET} {DIM}· this is {current}{RESET}"
        ),
        width,
        2,
    );
    for section in notes.iter().take(MAX_SECTIONS) {
        rows.push(String::new());
        rows.extend(wrap_words(
            &format!("{BOLD}{}{RESET}", text::sanitize_row(&section.heading)),
            width,
            2,
        ));
        let mut markdown = Markdown::new(width);
        let mut styled: Vec<String> = section
            .body
            .lines()
            .flat_map(|line| markdown.line(&text::sanitize_row(line)))
            .collect();
        styled.extend(markdown.finish());
        for row in styled {
            rows.extend(wrap_words(&row, width, text::hanging_indent(&row)));
        }
    }
    while rows.last().is_some_and(|row| text::width(row) == 0) {
        rows.pop();
    }
    let rest = notes.len().saturating_sub(MAX_SECTIONS);
    let more = match (notes.is_empty(), rest) {
        (true, _) => Some("What changed is in CHANGELOG.md.".to_string()),
        (false, 0) => None,
        (false, 1) => Some("… and 1 earlier release in CHANGELOG.md".to_string()),
        (false, n) => Some(format!("… and {n} earlier releases in CHANGELOG.md")),
    };
    if let Some(more) = more {
        rows.push(String::new());
        rows.extend(wrap_words(&format!("{DIM}{more}{RESET}"), width, 0));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::super::tests::unstyled;
    use super::*;

    fn section(version: &str, body: &str) -> Section {
        Section {
            heading: format!("{version} — 2026-11-09"),
            body: body.into(),
        }
    }

    #[test]
    fn the_notes_fit_and_only_the_newest_releases_are_printed() {
        let long = "- A change long enough to wrap at sixty columns, with `code` and **bold** in it, \
                    so the hanging indent is tested too.\n- A second item.";
        let notes: Vec<Section> = (0..5)
            .map(|i| section(&format!("9.9.{}", 9 - i), long))
            .collect();
        for width in [60, 80, 118] {
            let rows = release_rows("9.9.9", &notes, width);
            for row in &rows {
                assert!(text::width(row) <= width, "{width}: {row:?}");
            }
            let plain: Vec<String> = rows.iter().map(|r| unstyled(r)).collect();
            assert!(plain[0].starts_with("killi 9.9.9 is out · this is "), "{plain:?}");
            assert!(plain.iter().any(|r| r.contains("9.9.7 — 2026-11-09")));
            assert!(!plain.iter().any(|r| r.contains("9.9.6 —")), "{plain:?}");
            assert_eq!(
                plain.last().unwrap(),
                "… and 2 earlier releases in CHANGELOG.md"
            );
        }
    }

    fn found(app: &mut App, version: &str) {
        app.on_update_checked(Ok(Ok(Some(Release::sample(version)))));
    }

    #[test]
    fn a_release_waits_for_an_empty_prompt_and_is_asked_about_once() {
        let mut app = super::super::tests::test_app();
        app.editor.insert("half typed");
        found(&mut app, "9.9.9");
        assert!(app.picker.is_none(), "typing isn't interrupted");
        app.editor.clear();
        app.offer_update();
        let title = app.picker.as_ref().map(|p| p.title.clone()).unwrap();
        assert!(title.ends_with("→ 9.9.9?"), "{title}");
        assert!(app.updates.due.is_some(), "the next check is scheduled");

        app.picker = None;
        app.update_dismissed();
        found(&mut app, "9.9.9");
        assert!(app.picker.is_none(), "a declined version isn't asked about again");
        found(&mut app, "9.9.10");
        assert!(app.picker.is_some(), "a newer one is");

        app.picker = None;
        app.updates.asking = None;
        app.updates.installed = true;
        app.updates.due = None;
        found(&mut app, "9.9.11");
        assert!(
            app.picker.is_none() && app.updates.due.is_none(),
            "nothing more once one is installed"
        );
    }

    #[test]
    fn without_notes_the_offer_points_at_the_changelog() {
        let rows = release_rows("9.9.9", &[], 80);
        assert_eq!(
            unstyled(rows.last().unwrap()),
            "What changed is in CHANGELOG.md."
        );
    }
}
