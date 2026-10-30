use super::*;

#[derive(PartialEq)]
enum Mode {
    Content,
    Files,
    Count,
}

/// Line ranges (first, last; 0-based) of every match in `text`.
fn match_ranges(regex: &Regex, text: &str, lines: &[&str], multiline: bool) -> Vec<(usize, usize)> {
    if !multiline {
        return lines
            .iter()
            .enumerate()
            .filter(|(_, line)| regex.is_match(line))
            .map(|(n, _)| (n, n))
            .collect();
    }
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let line_of = |offset: usize| starts.partition_point(|&s| s <= offset).saturating_sub(1);
    regex
        .find_iter(text)
        .map(|m| {
            let last_line = lines.len().saturating_sub(1);
            let first = line_of(m.start()).min(last_line);
            (
                first,
                line_of(m.end().saturating_sub(1).max(m.start())).clamp(first, last_line),
            )
        })
        .collect()
}

/// Files `grep` didn't open because they were over [`MAX_FILE_SIZE`]; the note points at
/// bash, like `read`'s too-large advice.
#[derive(Default)]
struct SkippedLarge {
    count: usize,
    examples: Vec<String>,
}

impl SkippedLarge {
    fn record(&mut self, cwd: &Path, path: &Path) {
        self.count += 1;
        if self.examples.len() < 3 {
            self.examples
                .push(path.strip_prefix(cwd).unwrap_or(path).display().to_string());
        }
    }

    fn note(&self) -> Option<String> {
        (self.count > 0).then(|| {
            format!(
                "Skipped {} file{} over {} MB: {}{} — search them with bash (`rg`/`grep`).",
                self.count,
                if self.count == 1 { "" } else { "s" },
                MAX_FILE_SIZE / (1024 * 1024),
                self.examples.join(", "),
                if self.count > self.examples.len() {
                    " …"
                } else {
                    ""
                }
            )
        })
    }
}

fn text_reader(path: &Path) -> io::Result<Option<BufReader<File>>> {
    let mut reader = BufReader::new(File::open(path)?);
    if is_binary(reader.fill_buf()?) {
        return Ok(None);
    }
    Ok(Some(reader))
}

/// Reads lossily like `String::from_utf8_lossy(...).lines()`, without holding the whole file.
fn next_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<String>> {
    buf.clear();
    if reader.read_until(b'\n', buf)? == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Ok(Some(String::from_utf8_lossy(buf).into_owned()))
}

struct PreviousLine {
    number: usize,
    text: String,
    matched: bool,
}

impl PreviousLine {
    fn new(number: usize, text: String, matched: bool) -> Self {
        let text = if text.len() > MAX_MATCH_LINE {
            format!("{}… [truncated]", truncate(&text, MAX_MATCH_LINE))
        } else {
            text
        };
        Self {
            number,
            text,
            matched,
        }
    }
}

struct Pending {
    from: usize,
    to: usize,
    text: String,
}

fn push_line(out: &mut String, path: &str, line: &PreviousLine) {
    let sep = if line.matched { ':' } else { '-' };
    out.push_str(&format!(
        "{path}{sep}{}{sep} {}\n",
        line.number + 1,
        line.text
    ));
}

fn flush_pending(pending: &mut Option<Pending>, out: &mut String) -> Result<(), Stop> {
    let Some(pending) = pending.take() else {
        return Ok(());
    };
    if out.len() + pending.text.len() > MAX_BYTES {
        return Err(Stop::Bytes);
    }
    out.push_str(&pending.text);
    Ok(())
}

pub(super) fn stream_content(
    reader: &mut impl BufRead,
    regex: &Regex,
    context: usize,
    limit: usize,
    path: &str,
    out: &mut String,
    matches: &mut usize,
) -> io::Result<FileResult> {
    let mut buf = Vec::new();
    let mut previous: VecDeque<PreviousLine> = VecDeque::new();
    let mut pending: Option<Pending> = None;
    let mut printed: Option<usize> = None;
    let mut number = 0;
    let mut stop_after_pending = false;
    let mut stop = None;
    while !stop_after_pending || pending.is_some() {
        let Some(text) = next_line(reader, &mut buf)? else {
            break;
        };
        let matched = regex.is_match(&text);
        let line = PreviousLine::new(number, text, matched);
        let in_pending = pending
            .as_ref()
            .is_some_and(|chunk| (chunk.from..=chunk.to).contains(&number));
        if matched && !stop_after_pending {
            if *matches == limit {
                if let Some(chunk) = pending.as_mut() {
                    if in_pending {
                        push_line(&mut chunk.text, path, &line);
                    }
                    stop_after_pending = true;
                } else {
                    stop = Some(Stop::Limit);
                    break;
                }
            } else {
                *matches += 1;
                if let Some(chunk) = pending.as_mut() {
                    chunk.to = chunk.to.max(number + context);
                    printed = Some(printed.map_or(chunk.to, |p| p.max(chunk.to)));
                    if in_pending {
                        push_line(&mut chunk.text, path, &line);
                    }
                } else {
                    let from = number
                        .saturating_sub(context)
                        .max(printed.map_or(0, |p| p + 1));
                    let to = number + context;
                    let mut chunk = String::new();
                    if context > 0 && printed.is_some_and(|p| from > p + 1) && from <= to {
                        chunk.push_str("--\n");
                    }
                    for line in previous.iter().filter(|line| line.number >= from) {
                        push_line(&mut chunk, path, line);
                    }
                    if from <= number {
                        push_line(&mut chunk, path, &line);
                    }
                    printed = Some(printed.map_or(to, |p| p.max(to)));
                    if from <= to {
                        pending = Some(Pending {
                            from,
                            to,
                            text: chunk,
                        });
                    }
                }
            }
        } else if in_pending && let Some(chunk) = pending.as_mut() {
            push_line(&mut chunk.text, path, &line);
        }
        if pending.as_ref().is_some_and(|chunk| chunk.to == number)
            && let Err(stop) = flush_pending(&mut pending, out)
        {
            return Ok(FileResult {
                matches: 0,
                stop: Some(stop),
            });
        }
        if context > 0 {
            previous.push_back(line);
            if previous.len() > context {
                previous.pop_front();
            }
        }
        number += 1;
    }
    if let Err(failed) = flush_pending(&mut pending, out) {
        stop = Some(failed);
    } else if stop_after_pending {
        stop = Some(Stop::Limit);
    }
    Ok(FileResult { matches: 0, stop })
}

fn stream_matches(
    reader: &mut impl BufRead,
    regex: &Regex,
    files_only: bool,
) -> io::Result<FileResult> {
    let mut buf = Vec::new();
    let mut matches = 0;
    while let Some(line) = next_line(reader, &mut buf)? {
        if regex.is_match(&line) {
            matches += 1;
            if files_only {
                break;
            }
        }
    }
    Ok(FileResult {
        matches,
        stop: None,
    })
}

fn stop_note(stop: Stop, limit: usize) -> String {
    match stop {
        Stop::Limit => format!(
            "{limit} matches limit reached; use limit={} for more or refine the pattern.",
            limit * 2
        ),
        Stop::Bytes => "50KB limit reached; refine the pattern or path.".into(),
    }
}

pub(super) fn grep_blocking(cwd: &Path, root: &Path, args: &Value) -> Output {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Output::error("grep needs `pattern`.");
    };
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let multiline = flag("multiline");
    let regex = match RegexBuilder::new(pattern)
        .case_insensitive(flag("ignore_case"))
        .multi_line(multiline)
        .dot_matches_new_line(multiline)
        .build()
    {
        Ok(regex) => regex,
        Err(e) => return Output::error(format!("Invalid regex: {e}")),
    };
    let mode = match args.get("output").and_then(Value::as_str) {
        Some("files") => Mode::Files,
        Some("count") => Mode::Count,
        _ => Mode::Content,
    };
    let context = super::uint(args, "context").map_or(0, |c| c.min(MAX_CONTEXT) as usize);
    let limit = super::uint(args, "limit").map_or(DEFAULT_MATCHES, |l| l.max(1) as usize);
    let walk = match walker(root, args.get("glob").and_then(Value::as_str)) {
        Ok(walk) => walk,
        Err(message) => return Output::error(message),
    };

    let mut out = String::new();
    let mut matches = 0;
    let mut files = 0;
    let mut notes = Vec::new();
    let mut errors = SearchErrors::default();
    let mut skipped = SkippedLarge::default();
    let mut scanned_files = 0;
    let mut scanned_bytes = 0_u64;
    let mut scan_limited = false;
    'files: for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = walk_error_path(&error).unwrap_or(root).to_path_buf();
                errors.record(cwd, &path, error);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.record(cwd, entry.path(), error);
                continue;
            }
        };
        if metadata.len() > MAX_FILE_SIZE {
            skipped.record(cwd, entry.path());
            continue;
        }
        if scanned_files == MAX_SCAN_FILES
            || scanned_bytes.saturating_add(metadata.len()) > MAX_SCAN_BYTES
        {
            scan_limited = true;
            break;
        }
        scanned_files += 1;
        scanned_bytes += metadata.len();
        let shown_path = entry
            .path()
            .strip_prefix(cwd)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        if multiline {
            let bytes = match std::fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(error) => {
                    errors.record(cwd, entry.path(), error);
                    continue;
                }
            };
            if is_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let ranges = match_ranges(&regex, &text, &lines, true);
            if ranges.is_empty() || lines.is_empty() {
                continue;
            }
            if mode != Mode::Content {
                if files == limit {
                    notes.push(format!(
                        "{limit} files limit reached; use limit={} for more or refine the pattern.",
                        limit * 2
                    ));
                    break;
                }
                files += 1;
                matches += ranges.len();
                out.push_str(&if mode == Mode::Files {
                    format!("{shown_path}\n")
                } else {
                    format!("{shown_path}:{}\n", ranges.len())
                });
                continue;
            }
            let mut is_match = vec![false; lines.len()];
            for &(first, last) in &ranges {
                is_match[first..=last.min(lines.len().saturating_sub(1))].fill(true);
            }
            let mut printed: Option<usize> = None;
            for &(first, last) in &ranges {
                if matches == limit {
                    notes.push(stop_note(Stop::Limit, limit));
                    break 'files;
                }
                matches += 1;
                let from = first
                    .saturating_sub(context)
                    .max(printed.map_or(0, |p| p + 1));
                let to = (last + context).min(lines.len().saturating_sub(1));
                let mut chunk = String::new();
                if context > 0 && printed.is_some_and(|p| from > p + 1) && from <= to {
                    chunk.push_str("--\n");
                }
                for n in from..=to {
                    let line = PreviousLine::new(n, lines[n].to_string(), is_match[n]);
                    push_line(&mut chunk, &shown_path, &line);
                }
                if out.len() + chunk.len() > MAX_BYTES {
                    notes.push(stop_note(Stop::Bytes, limit));
                    break 'files;
                }
                out.push_str(&chunk);
                printed = Some(printed.map_or(to, |p| p.max(to)));
            }
            continue;
        }
        let mut reader = match text_reader(entry.path()) {
            Ok(Some(reader)) => reader,
            Ok(None) => continue,
            Err(error) => {
                errors.record(cwd, entry.path(), error);
                continue;
            }
        };
        let result = if mode == Mode::Content {
            stream_content(
                &mut reader,
                &regex,
                context,
                limit,
                &shown_path,
                &mut out,
                &mut matches,
            )
        } else {
            stream_matches(&mut reader, &regex, mode == Mode::Files)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                errors.record(cwd, entry.path(), error);
                continue;
            }
        };
        if let Some(stop) = result.stop {
            notes.push(stop_note(stop, limit));
            break;
        }
        if mode != Mode::Content {
            if result.matches == 0 {
                continue;
            }
            if files == limit {
                notes.push(format!(
                    "{limit} files limit reached; use limit={} for more or refine the pattern.",
                    limit * 2
                ));
                break;
            }
            files += 1;
            matches += result.matches;
            out.push_str(&if mode == Mode::Files {
                format!("{shown_path}\n")
            } else {
                format!("{shown_path}:{}\n", result.matches)
            });
        }
    }
    if scan_limited {
        notes.push(format!("Search incomplete: internal scan work limit reached ({MAX_SCAN_FILES} files or 1GiB); narrow the pattern or path."));
    }
    if let Some(note) = skipped.note() {
        notes.push(note);
    }
    if let Some(note) = errors.note() {
        notes.push(note);
    }
    let incomplete = scan_limited || errors.count > 0 || skipped.count > 0;
    if matches == 0 {
        let mut text = "No matches found.".to_string();
        if !notes.is_empty() {
            text.push_str(&format!("\n[{}]", notes.join(" ")));
        }
        return if incomplete {
            Output::error(text)
        } else {
            Output::ok(text)
        };
    }
    if !notes.is_empty() {
        out.push_str(&format!("[{}]", notes.join(" ")));
    }
    let brief = match mode {
        Mode::Content => format!("{matches} match{}", if matches == 1 { "" } else { "es" }),
        _ => format!("{files} file{}", if files == 1 { "" } else { "s" }),
    };
    Output::ok(out.trim_end().to_string()).brief(brief)
}
