//! The slash menu: what `/` offers as you type, at the start of a word anywhere in the prompt. Commands and skills are ranked with pi-tui's
//! fuzzy matcher, as in Kimi Code, then entries whose description mentions what you typed
//! follow; after `/model ` or `/effort ` it offers that command's choices. `@` at the start of
//! a word offers the workspace's files the same way.

/// One row of the menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// What a choice inserts: a command name without its slash, an argument, or a path.
    pub value: String,
    pub description: String,
}

/// A command or skill the menu can offer, with the other names it answers to.
#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
}

/// What the menu offers for the text before the cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestions {
    pub items: Vec<Item>,
    /// Byte offset in the text where the replaced part starts.
    pub start: usize,
    pub fill: Fill,
}

/// What choosing an item puts in the prompt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    /// A command or skill name, as `/name `.
    Command,
    /// A command's argument, in place of the one typed.
    Argument,
    /// A workspace path, as `@path `; a folder ends in `/` with no space, to keep typing in it.
    File,
}

/// Files the `@` menu lists at most; the rest are reached by typing more.
const MAX_FILES_SHOWN: usize = 200;

/// The menu for `before`, the text before the cursor, or None when there is nothing to offer.
/// `arguments` gives a command's choices by its name.
///
/// A `/` that starts a word anywhere in the text offers commands and skills; only the one at
/// the very start can be followed by that command's choices.
pub fn suggest(
    before: &str,
    entries: &[Entry],
    arguments: impl Fn(&str) -> Vec<Item>,
) -> Option<Suggestions> {
    let word = word_start(before);
    if let Some(typed) = before[word..].strip_prefix('/') {
        let items = commands(entries, typed);
        // A complete name is typed: no menu, so Enter sends the prompt (as the `@` menu gives
        // none for the one path it matches) — a skill runs at once, also while the agent works.
        if items.first().is_some_and(|item| item.value == typed) {
            return None;
        }
        return (!items.is_empty()).then_some(Suggestions {
            items,
            start: word,
            fill: Fill::Command,
        });
    }
    let typed = before.strip_prefix('/')?;
    let (space, space_char) = typed.char_indices().find(|(_, c)| c.is_whitespace())?;
    let (name, argument) = (&typed[..space], &typed[space + space_char.len_utf8()..]);
    if argument.contains(char::is_whitespace) {
        return None;
    }
    let choices = arguments(name);
    let mut items = fuzzy_filter(&choices, argument, |item| &item.value);
    if items.len() == 1 && items[0].value == argument {
        items.clear();
    }
    (!items.is_empty()).then_some(Suggestions {
        items,
        start: 1 + space + space_char.len_utf8(),
        fill: Fill::Argument,
    })
}

/// The file menu for `before`: the `files` of the workspace that fuzzy-match the word at the
/// cursor after its `@`, best first, or None when the word isn't one or nothing matches. A
/// word that already is the one path it matches gets no menu, so Enter sends the prompt.
pub fn files(before: &str, files: &[String]) -> Option<Suggestions> {
    let start = word_start(before);
    let typed = before[start..].strip_prefix('@')?;
    let paths: Vec<&str> = files.iter().map(String::as_str).collect();
    let matched = fuzzy_filter(&paths, typed, |path| path);
    if matched.is_empty() || matched == [typed] {
        return None;
    }
    let items = matched
        .into_iter()
        .take(MAX_FILES_SHOWN)
        .map(|path| Item {
            value: path.to_string(),
            description: String::new(),
        })
        .collect();
    Some(Suggestions {
        items,
        start,
        fill: Fill::File,
    })
}

/// The byte offset where the word the cursor is in begins.
fn word_start(before: &str) -> usize {
    before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// Entries whose name or alias fuzzy-matches `query`, best first, each shown under the name
/// that matched best; then entries whose description contains it.
fn commands(entries: &[Entry], query: &str) -> Vec<Item> {
    let tokens: Vec<&str> = query
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return entries
            .iter()
            .map(|e| Item {
                value: e.name.clone(),
                description: e.description.clone(),
            })
            .collect();
    }
    let mut scored: Vec<(f64, Item)> = Vec::new();
    let mut rest = Vec::new();
    for entry in entries {
        let best = std::iter::once(&entry.name)
            .chain(&entry.aliases)
            .filter_map(|name| Some((score_tokens(&tokens, name)?, name)))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((score, name)) => scored.push((
                score,
                Item {
                    value: name.clone(),
                    description: entry.description.clone(),
                },
            )),
            None => rest.push(entry),
        }
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut items: Vec<Item> = scored.into_iter().map(|(_, item)| item).collect();
    let needle = query.trim().to_lowercase();
    if needle.chars().count() >= 2 {
        items.extend(
            rest.into_iter()
                .filter(|e| e.description.to_lowercase().contains(&needle))
                .map(|e| Item {
                    value: e.name.clone(),
                    description: e.description.clone(),
                }),
        );
    }
    items
}

/// pi-tui's `fuzzyFilter`: tokens split on whitespace and `/` must all match; best first.
pub fn fuzzy_filter<'a, T: Clone>(
    items: &'a [T],
    query: &str,
    text: impl Fn(&'a T) -> &'a str,
) -> Vec<T> {
    let tokens: Vec<&str> = query
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.to_vec();
    }
    let mut scored: Vec<(f64, &T)> = items
        .iter()
        .filter_map(|item| Some((score_tokens(&tokens, text(item))?, item)))
        .collect();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    scored.into_iter().map(|(_, item)| item.clone()).collect()
}

fn score_tokens(tokens: &[&str], text: &str) -> Option<f64> {
    tokens.iter().map(|token| fuzzy_match(token, text)).sum()
}

/// pi-tui's `fuzzyMatch`: every character of `query` appears in `text` in order. Lower is
/// better: consecutive runs and word starts score well, gaps and late matches cost. A query
/// like `v2` also matches with its letters and digits swapped.
pub fn fuzzy_match(query: &str, text: &str) -> Option<f64> {
    let query = query.to_lowercase();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    if let Some(score) = match_chars(&query.chars().collect::<Vec<_>>(), &text) {
        return Some(score);
    }
    let split = query
        .find(|c: char| c.is_ascii_digit() != query.starts_with(|f: char| f.is_ascii_digit()))?;
    let (head, tail) = query.split_at(split);
    let letters_digits = |a: &str, b: &str| {
        a.chars().all(|c| c.is_ascii_lowercase()) && b.chars().all(|c| c.is_ascii_digit())
    };
    if !(letters_digits(head, tail) || letters_digits(tail, head)) {
        return None;
    }
    let swapped: Vec<char> = tail.chars().chain(head.chars()).collect();
    match_chars(&swapped, &text).map(|score| score + 5.0)
}

fn match_chars(query: &[char], text: &[char]) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    if query.len() > text.len() {
        return None;
    }
    let (mut matched, mut score, mut last, mut run) = (0, 0.0, -1isize, 0);
    for (i, &c) in text.iter().enumerate() {
        if matched == query.len() {
            break;
        }
        if c != query[matched] {
            continue;
        }
        let i = i as isize;
        if last == i - 1 {
            run += 1;
            score -= f64::from(run) * 5.0;
        } else {
            run = 0;
            if last >= 0 {
                score += (i - last - 1) as f64 * 2.0;
            }
        }
        if i == 0
            || matches!(
                text[i as usize - 1],
                ' ' | '\t' | '\n' | '-' | '_' | '.' | '/' | ':'
            )
        {
            score -= 10.0;
        }
        score += i as f64 * 0.1;
        last = i;
        matched += 1;
    }
    if matched < query.len() {
        return None;
    }
    if query == text {
        score -= 100.0;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, aliases: &[&str], description: &str) -> Entry {
        Entry {
            name: name.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            description: description.into(),
        }
    }

    fn values(s: &Suggestions) -> Vec<&str> {
        s.items.iter().map(|i| i.value.as_str()).collect()
    }

    fn entries() -> Vec<Entry> {
        vec![
            entry("model", &[], "Switch model"),
            entry("effort", &["thinking"], "Switch thinking effort"),
            entry("new", &["clear"], "Start a fresh session"),
            entry("exit", &["quit"], "Quit killi"),
            entry("code-review", &[], "Review the diff for bugs"),
            entry("pdf", &[], "Read and write PDF files"),
            entry("make-pdf-report", &[], "Reports"),
        ]
    }

    #[test]
    fn a_slash_alone_lists_everything_in_order() {
        let s = suggest("/", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(
            values(&s),
            [
                "model",
                "effort",
                "new",
                "exit",
                "code-review",
                "pdf",
                "make-pdf-report"
            ]
        );
        assert_eq!(s.fill, Fill::Command);
    }

    #[test]
    fn fuzzy_matches_anywhere_in_the_name_and_ranks_tight_matches_first() {
        let s = suggest("/rev", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s)[0], "code-review");
        let s = suggest("/pd", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s)[..2], ["pdf", "make-pdf-report"]);
        let s = suggest("/cr", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s)[0], "code-review");
    }

    #[test]
    fn an_alias_shows_under_its_own_name() {
        let s = suggest("/clea", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s), ["clear"]);
        let s = suggest("/think", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s)[0], "thinking");
    }

    #[test]
    fn descriptions_match_after_names() {
        let s = suggest("/bugs", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s), ["code-review"]);
        assert!(suggest("/zzz", &entries(), |_| Vec::new()).is_none());
    }

    #[test]
    fn a_name_typed_in_full_gets_no_menu_so_enter_sends() {
        let none = |_: &str| Vec::new();
        assert!(
            suggest("/pdf", &entries(), none).is_none(),
            "a skill typed in full sends"
        );
        assert!(
            suggest("/exit", &entries(), none).is_none(),
            "a command typed in full sends"
        );
        assert!(
            suggest("/clear", &entries(), none).is_none(),
            "an alias typed in full sends"
        );
        assert!(suggest("use /pdf now", &entries(), none).is_none());
        assert!(
            suggest("/pd", &entries(), none).is_some(),
            "a prefix still completes"
        );
        assert!(
            suggest("/pdf ", &entries(), none).is_none(),
            "a skill has no argument choices"
        );
    }

    #[test]
    fn arguments_complete_after_the_command() {
        let efforts = |name: &str| match name {
            "effort" => ["off", "low", "medium", "high"]
                .iter()
                .map(|e| Item {
                    value: e.to_string(),
                    description: String::new(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let s = suggest("/effort h", &entries(), efforts).unwrap();
        assert_eq!(values(&s), ["high"]);
        assert_eq!((s.start, s.fill), (8, Fill::Argument));
        assert_eq!(
            values(&suggest("/effort ", &entries(), efforts).unwrap()).len(),
            4
        );
        assert!(suggest("/effort high", &entries(), efforts).is_none());
        assert!(suggest("/model x", &entries(), efforts).is_none());
        assert!(suggest("/effort high now", &entries(), efforts).is_none());
    }

    #[test]
    fn a_later_slash_offers_commands_where_it_stands() {
        let none = |_: &str| Vec::new();
        let s = suggest("use /pdf and /rev", &entries(), none).unwrap();
        assert_eq!(values(&s), ["code-review"]);
        assert_eq!((s.start, s.fill), (13, Fill::Command));
        assert_eq!(suggest("/pdf then /", &entries(), none).unwrap().start, 10);
        assert_eq!(suggest("héllo /pd", &entries(), none).unwrap().start, 7);
        assert!(suggest("see src/pdf", &entries(), none).is_none());
        assert!(suggest("hello /zzz", &entries(), none).is_none());
        assert!(suggest("hello ", &entries(), none).is_none());
    }

    #[test]
    fn an_at_word_offers_the_files_that_match_it() {
        let paths: Vec<String> = ["README.md", "src/main.rs", "src/tui/", "src/tui/mod.rs"]
            .iter()
            .map(|p| p.to_string())
            .collect();
        let s = files("see @", &paths).unwrap();
        assert_eq!((values(&s).len(), s.start, s.fill), (4, 4, Fill::File));
        assert_eq!(
            values(&files("@tuimod", &paths).unwrap()),
            ["src/tui/mod.rs"]
        );
        assert_eq!(values(&files("@tui/", &paths).unwrap())[0], "src/tui/");
        assert!(
            files("@src/main.rs", &paths).is_none(),
            "the path typed in full needs no menu"
        );
        assert!(files("@zzz", &paths).is_none());
        assert!(
            files("mail me@", &paths).is_none(),
            "an @ inside a word is not a mention"
        );
        assert!(files("hello", &paths).is_none());
    }

    #[test]
    fn nbsp_and_ideographic_space_after_a_command() {
        let efforts = |name: &str| match name {
            "effort" => ["off", "high"]
                .iter()
                .map(|e| Item {
                    value: e.to_string(),
                    description: String::new(),
                })
                .collect(),
            _ => Vec::new(),
        };
        for space in ['\u{a0}', '\u{3000}'] {
            let before = format!("/effort{space}hig");
            let s = suggest(&before, &entries(), efforts).unwrap();
            assert_eq!(values(&s), ["high"]);
            assert_eq!(
                (s.start, s.fill),
                (before.len() - "hig".len(), Fill::Argument)
            );
        }
    }

    #[test]
    fn pi_tui_scores() {
        assert!(fuzzy_match("abc", "xaxbxc").is_some());
        assert!(fuzzy_match("abd", "abc").is_none());
        assert!(fuzzy_match("model", "model").unwrap() < fuzzy_match("model", "models").unwrap());
        assert!(fuzzy_match("v26", "mimo-v2.6-pro").is_some());
        assert!(fuzzy_match("2v", "v2").is_some());
    }
}
