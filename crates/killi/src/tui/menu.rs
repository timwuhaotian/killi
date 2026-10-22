//! The slash menu: what `/` offers as you type. Commands and skills are ranked with pi-tui's
//! fuzzy matcher, as in Kimi Code, then entries whose description mentions what you typed
//! follow; after `/model ` or `/effort ` it offers that command's choices.

/// One row of the menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// What a choice inserts: a command name without its slash, or an argument.
    pub value: String,
    pub description: String,
}

/// A command or skill the menu can offer, with the other names it answers to.
pub struct Entry {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
}

/// What the menu offers for the text before the cursor.
#[derive(Debug, PartialEq)]
pub struct Suggestions {
    pub items: Vec<Item>,
    /// Byte offset in the text where the replaced part starts.
    pub start: usize,
    /// Choosing replaces the command name (`/name `) rather than its argument.
    pub command: bool,
}

/// The menu for `before`, the text before the cursor, or None when there is nothing to offer.
/// `arguments` gives a command's choices by its name.
pub fn suggest(before: &str, entries: &[Entry], arguments: impl Fn(&str) -> Vec<Item>) -> Option<Suggestions> {
    let typed = before.strip_prefix('/')?;
    let Some(space) = typed.find(char::is_whitespace) else {
        let items = commands(entries, typed);
        return (!items.is_empty()).then_some(Suggestions { items, start: 0, command: true });
    };
    let (name, argument) = (&typed[..space], &typed[space + 1..]);
    if argument.contains(char::is_whitespace) {
        return None;
    }
    let choices = arguments(name);
    let mut items = fuzzy_filter(&choices, argument, |item| &item.value);
    if items.len() == 1 && items[0].value == argument {
        items.clear();
    }
    (!items.is_empty()).then_some(Suggestions { items, start: 1 + space + 1, command: false })
}

/// Entries whose name or alias fuzzy-matches `query`, best first, each shown under the name
/// that matched best; then entries whose description contains it.
fn commands(entries: &[Entry], query: &str) -> Vec<Item> {
    let tokens: Vec<&str> = query.split(|c: char| c.is_whitespace() || c == '/').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return entries.iter().map(|e| Item { value: e.name.clone(), description: e.description.clone() }).collect();
    }
    let mut scored: Vec<(f64, Item)> = Vec::new();
    let mut rest = Vec::new();
    for entry in entries {
        let best = std::iter::once(&entry.name)
            .chain(&entry.aliases)
            .filter_map(|name| Some((score_tokens(&tokens, name)?, name)))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((score, name)) => scored.push((score, Item { value: name.clone(), description: entry.description.clone() })),
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
                .map(|e| Item { value: e.name.clone(), description: e.description.clone() }),
        );
    }
    items
}

/// pi-tui's `fuzzyFilter`: tokens split on whitespace and `/` must all match; best first.
pub fn fuzzy_filter<'a, T: Clone>(items: &'a [T], query: &str, text: impl Fn(&'a T) -> &'a str) -> Vec<T> {
    let tokens: Vec<&str> = query.split(|c: char| c.is_whitespace() || c == '/').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return items.to_vec();
    }
    let mut scored: Vec<(f64, &T)> = items.iter().filter_map(|item| Some((score_tokens(&tokens, text(item))?, item))).collect();
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
    let split = query.find(|c: char| c.is_ascii_digit() != query.starts_with(|f: char| f.is_ascii_digit()))?;
    let (head, tail) = query.split_at(split);
    let letters_digits = |a: &str, b: &str| a.chars().all(|c| c.is_ascii_lowercase()) && b.chars().all(|c| c.is_ascii_digit());
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
        if i == 0 || matches!(text[i as usize - 1], ' ' | '\t' | '\n' | '-' | '_' | '.' | '/' | ':') {
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
        Entry { name: name.into(), aliases: aliases.iter().map(|a| a.to_string()).collect(), description: description.into() }
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
        assert_eq!(values(&s), ["model", "effort", "new", "exit", "code-review", "pdf", "make-pdf-report"]);
        assert!(s.command);
    }

    #[test]
    fn fuzzy_matches_anywhere_in_the_name_and_ranks_tight_matches_first() {
        let s = suggest("/rev", &entries(), |_| Vec::new()).unwrap();
        assert_eq!(values(&s)[0], "code-review");
        let s = suggest("/pdf", &entries(), |_| Vec::new()).unwrap();
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
    fn arguments_complete_after_the_command() {
        let efforts = |name: &str| match name {
            "effort" => ["off", "low", "medium", "high"].iter().map(|e| Item { value: e.to_string(), description: String::new() }).collect(),
            _ => Vec::new(),
        };
        let s = suggest("/effort h", &entries(), efforts).unwrap();
        assert_eq!(values(&s), ["high"]);
        assert_eq!((s.start, s.command), (8, false));
        assert_eq!(values(&suggest("/effort ", &entries(), efforts).unwrap()).len(), 4);
        assert!(suggest("/effort high", &entries(), efforts).is_none());
        assert!(suggest("/model x", &entries(), efforts).is_none());
        assert!(suggest("/effort high now", &entries(), efforts).is_none());
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
