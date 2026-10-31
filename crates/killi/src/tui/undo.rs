use super::*;

/// Whether /undo has nothing to go back to: no snapshot was ever taken, and snapshots
/// aren't off. That reads as a notice, not an error.
pub(super) fn undo_empty(snapshots: &Snapshots) -> bool {
    snapshots.last_taken().is_none() && snapshots.disabled.is_none()
}

/// Paths from `git diff --name-status` lines: the path after the status.
pub(super) fn undo_paths(changes: &[String]) -> Vec<String> {
    changes
        .iter()
        .filter_map(|change| {
            let change = change.trim();
            if change.is_empty() {
                return None;
            }
            if let Some((_, path)) = change.rsplit_once('\t') {
                let path = path.trim();
                return (!path.is_empty()).then(|| path.to_string());
            }
            match change.split_once(' ') {
                Some((_, path)) => {
                    let path = path.trim();
                    (!path.is_empty()).then(|| path.to_string())
                }
                None => Some(change.to_string()),
            }
        })
        .collect()
}

/// What the model hears after /undo instead of the interruption note: what was restored,
/// so it reads the files again instead of relying on what it saw.
pub(super) fn undo_note(first_line: &str, paths: &[String]) -> String {
    const MAX_PATHS: usize = 30;
    let listed = if paths.len() > MAX_PATHS {
        format!(
            "{}, and {} more",
            paths[..MAX_PATHS].join(", "),
            paths.len() - MAX_PATHS
        )
    } else {
        paths.join(", ")
    };
    format!(
        "The user ran /undo: files were restored to how they were before \"{first_line}\". Changed back: {listed}. Read files again before relying on what you saw."
    )
}

impl App {
    /// Prints the held prompt into the conversation, once something of its turn is printed.
    pub(super) fn release_prompt(&mut self) {
        if let Some((prompt, _)) = self.held_prompt.take() {
            self.commit_prompt(&prompt);
        }
    }

    /// Esc before anything of the turn was printed and before the reply began: the prompt goes
    /// back into the editor ahead of what was queued and typed meanwhile, and the agent forgets
    /// it, as if it was never sent. A thought in progress goes with it. False, changing nothing,
    /// when something showed or the agent can no longer take the prompt back.
    pub(super) fn take_back(&mut self, agent: &mut Agent) -> Result<bool> {
        if self.held_prompt.is_none()
            || self.send_now
            || !self.partial.trim().is_empty()
            || !agent.retract()?
        {
            return Ok(false);
        }
        let (prompt, images) = self.held_prompt.take().expect("a held prompt");
        self.thinking = None;
        let _ = self.reasoning.finish();
        self.end_busy();
        let typed = std::mem::take(&mut self.queued);
        let queued = agent
            .take_steered()
            .into_iter()
            .enumerate()
            .map(|(i, (expanded, images))| (typed.get(i).cloned().unwrap_or(expanded), images));
        let draft = self.editor.take_draft();
        let mut back = vec![(prompt, images)];
        back.extend(queued);
        back.extend(Some(draft).filter(|(text, _)| !text.is_empty()));
        restore_steered(&mut self.editor, back);
        self.last_end = None;
        Ok(true)
    }
}

/// Puts messages typed during a finished turn back into the prompt, blank-line separated,
/// with their images where they were pasted.
pub(super) fn restore_steered(editor: &mut Editor, steered: Vec<Steered>) {
    let mut first = editor.is_empty();
    for (text, images) in steered {
        if !first {
            editor.insert("\n\n");
        }
        first = false;
        insert_with_images(editor, &text, &images);
    }
}

/// Inserts `text`, re-attaching each image where its placeholder stood so the editor holds
/// the image itself, not just its old label. Images whose placeholders are gone attach last.
fn insert_with_images(editor: &mut Editor, text: &str, images: &[Image]) {
    let mut rest = text;
    let mut placed = vec![false; images.len()];
    loop {
        let mut next: Option<(usize, usize)> = None;
        for (i, image) in images.iter().enumerate() {
            if placed[i] {
                continue;
            }
            if let Some(at) = rest.find(image.label.as_str()) {
                let before = next.is_none_or(|(best, _)| at < best);
                if before {
                    next = Some((at, i));
                }
            }
        }
        let Some((at, i)) = next else { break };
        editor.insert(&rest[..at]);
        editor.attach(images[i].url.clone());
        rest = &rest[at + images[i].label.len()..];
        placed[i] = true;
    }
    editor.insert(rest);
    for (image, done) in images.iter().zip(placed) {
        if !done {
            editor.attach(image.url.clone());
        }
    }
}
