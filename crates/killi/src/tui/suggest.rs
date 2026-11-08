//! What the prompt could become, from the model: what to ask next once a turn ends, and how a
//! draft goes on once typing pauses or the last suggestion was taken whole. Each is a side
//! request like `/btw`, never kept in the conversation, so the cached prefix still hits.

use super::*;

/// How long typing must pause before the draft is sent for a continuation.
pub(super) const COMPLETE_PAUSE: Duration = Duration::from_millis(1000);

impl App {
    /// The part of the suggestion still ahead of what is typed: the empty prompt's placeholder,
    /// or faint text after a draft. None unless the cursor is at the end of the prompt and the
    /// agent waits for it.
    pub(super) fn ghost(&self) -> Option<&str> {
        if self.busy.is_some()
            || self.asking.is_some()
            || self.picker.is_some()
            || !self.editor.at_end()
        {
            return None;
        }
        let rest = self
            .suggestion
            .as_deref()?
            .strip_prefix(self.editor.text())?;
        (!rest.is_empty()).then_some(rest)
    }

    /// Asks the model what the prompt as it stands could become: what to ask next when it is
    /// empty, else the draft continued. A draft that is a command, a shell line or a history
    /// entry, or that a menu is completing, isn't asked about.
    pub(super) fn ask_suggestion(&mut self) {
        let text = self.editor.text();
        let draft = !text.trim().is_empty()
            && !text.starts_with('/')
            && shell::shell_arg(text).is_none()
            && !self.editor.browsing();
        if !self.editor.is_empty() && !draft {
            return;
        }
        if self.busy.is_some()
            || self.asking.is_some()
            || self.picker.is_some()
            || !self.editor.at_end()
            || self.suggestions().is_some()
        {
            return;
        }
        let ask = self.controls.aside.borrow().suggest(self.editor.text());
        self.suggesting = Some(Box::pin(ask));
    }

    /// After the prompt's text changed: a suggestion on its way answers text that is gone. A
    /// suggestion that still leads on from the text stays; one taken whole asks at once how it
    /// goes on, and any other edit asks after a pause.
    pub(super) fn edited(&mut self) {
        self.suggesting = None;
        let text = self.editor.text();
        self.complete_at = if text.trim().is_empty() || self.ghost().is_some() {
            None
        } else if self.suggestion.as_deref() == Some(text) {
            Some(Instant::now())
        } else {
            Some(Instant::now() + COMPLETE_PAUSE)
        };
    }

    /// Tab or → at the end of the prompt takes the part of the suggestion still ahead of it;
    /// true when it did. With nothing ahead the keys keep their other meanings.
    pub(super) fn fill_suggestion(&mut self, key: &KeyEvent) -> bool {
        if !matches!(key.code, KeyCode::Tab | KeyCode::Right) || !key.modifiers.is_empty() {
            return false;
        }
        let Some(ghost) = self.ghost().map(str::to_string) else {
            return false;
        };
        self.editor.insert(&ghost);
        true
    }

    /// Forgets the suggestion, the one on its way and a continuation that is due: they belong
    /// to a turn or conversation that is over.
    pub(super) fn drop_suggestion(&mut self) {
        self.suggestion = None;
        self.suggesting = None;
        self.complete_at = None;
    }
}
