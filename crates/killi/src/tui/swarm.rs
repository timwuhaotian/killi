//! The live view of an `agent_swarm` call: how many subagents there are, how many have
//! finished, and for each one whether it is waiting, working (and on what) or done. While the
//! call runs its subagents' calls stay out of the conversation; each one that ends leaves a
//! single line there.

use std::time::{Duration, Instant};

use super::banner;
use super::text::{self, BOLD, DANGER, DIM, FAINT, LAGOON, RESET};
use super::{QUARTER, clip, clock, fit, tool_text};

/// The longest a subagent's name takes in the panel.
const NAME_MAX: usize = 24;
/// The most rows the strip of one cell per subagent takes.
const STRIP_ROWS: usize = 3;
/// The columns the header's meter takes.
const METER: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Queued,
    Running,
    Done,
    Failed,
}

struct Member {
    item: String,
    state: State,
    started: Option<Instant>,
    ended: Option<Instant>,
    /// Calls it has made, and the last one's summary.
    calls: usize,
    doing: String,
}

pub struct Swarm {
    /// The `agent_swarm` call's tool id.
    pub id: String,
    description: String,
    started: Instant,
    members: Vec<Member>,
}

impl Swarm {
    pub fn new(id: String, description: String, items: Vec<String>) -> Self {
        let members = items
            .iter()
            .map(|item| Member {
                item: item.split_whitespace().collect::<Vec<_>>().join(" "),
                state: State::Queued,
                started: None,
                ended: None,
                calls: 0,
                doing: String::new(),
            })
            .collect();
        Self { id, description, started: Instant::now(), members }
    }

    /// Subagent `index` began; a later signal from one that hasn't been seen start counts too.
    pub fn start(&mut self, index: usize) {
        if let Some(member) = self.members.get_mut(index).filter(|m| m.state == State::Queued) {
            member.state = State::Running;
            member.started = Some(Instant::now());
        }
    }

    /// Subagent `index` made a call.
    pub fn call(&mut self, index: usize, summary: &str) {
        self.start(index);
        if let Some(member) = self.members.get_mut(index) {
            member.calls += 1;
            member.doing = tool_text(summary);
        }
    }

    /// Subagent `index` ended: its name, calls and time, for the line it leaves behind.
    pub fn finish(&mut self, index: usize, failed: bool) -> Option<(String, usize, Duration)> {
        self.start(index);
        let member = self.members.get_mut(index)?;
        let now = Instant::now();
        member.state = if failed { State::Failed } else { State::Done };
        member.ended = Some(now);
        let took = now.duration_since(member.started.unwrap_or(now));
        Some((member.item.clone(), member.calls, took))
    }

    fn count(&self, state: State) -> usize {
        self.members.iter().filter(|m| m.state == state).count()
    }

    /// The panel in at most `budget` rows: the header, then a row for every subagent. When
    /// they don't all fit, a strip of one cell each stands for all of them and the rows are
    /// for the ones working.
    pub fn rows(&self, width: usize, frame: usize, budget: usize) -> Vec<String> {
        let mut rows = vec![self.header(width, frame)];
        let mut room = budget.saturating_sub(1);
        let name = self.members.iter().map(|m| text::width(&m.item)).max().unwrap_or(0).clamp(6, NAME_MAX).min(width.saturating_sub(26) / 2).max(6);
        if self.members.len() <= room {
            rows.extend(self.members.iter().map(|m| member_row(m, width, frame, name)));
            return rows;
        }
        let strip = self.strip(width, frame);
        room = room.saturating_sub(strip.len());
        rows.extend(strip);
        let working: Vec<&Member> = self.members.iter().filter(|m| m.state == State::Running).collect();
        let shown = if working.len() <= room { working.len() } else { room.saturating_sub(1) };
        rows.extend(working.iter().take(shown).map(|m| member_row(m, width, frame, name)));
        if working.len() > shown {
            rows.push(format!("  {DIM}… {} more working{RESET}", working.len() - shown));
        }
        rows
    }

    /// `◐ swarm Review PRs  ━━━━───── 3/5 done · 2 running · 9m49s`, the parts after the
    /// description dropping out as the window narrows so the count stays.
    fn header(&self, width: usize, frame: usize) -> String {
        let (total, running, waiting, failed) = (self.members.len(), self.count(State::Running), self.count(State::Queued), self.count(State::Failed));
        let finished = total - running - waiting;
        let counts = |with_waiting: bool| {
            let mut parts = vec![format!("{finished}/{total} done")];
            if running > 0 {
                parts.push(format!("{running} running"));
            }
            if with_waiting && waiting > 0 {
                parts.push(format!("{waiting} queued"));
            }
            if failed > 0 {
                parts.push(format!("{DANGER}{failed} failed{RESET}{DIM}"));
            }
            parts.push(clock(self.started.elapsed()));
            format!("{DIM}{}{RESET}", parts.join(&format!(" {FAINT}·{RESET}{DIM} ")))
        };
        let meter = banner::bar(finished as f32 / total.max(1) as f32, METER);
        let lead = format!("{LAGOON}{}{RESET} {BOLD}swarm{RESET} ", QUARTER[frame % QUARTER.len()]);
        let tails = [format!(" {meter} {}", counts(true)), format!(" {meter} {}", counts(false)), format!(" {}", counts(false))];
        let wanted = text::width(&self.description).min(NAME_MAX + 4);
        let left = |tail: &String| width.saturating_sub(text::width(&lead) + text::width(tail));
        // The meter is worth a cut description, the queue count is not.
        let tail = if left(&tails[0]) >= wanted {
            &tails[0]
        } else if left(&tails[1]) >= wanted.min(10) {
            &tails[1]
        } else {
            &tails[2]
        };
        let room = left(tail).max(8);
        fit(&format!("{lead}{}{tail}", clip(&self.description, room)), width)
    }

    /// One cell per subagent in item order, wrapped under the header.
    fn strip(&self, width: usize, frame: usize) -> Vec<String> {
        let cells: Vec<String> = self.members.iter().map(|m| glyph(m.state, frame)).collect();
        cells.chunks(width.saturating_sub(4).max(1)).take(STRIP_ROWS).map(|row| format!("  {}", row.concat())).collect()
    }
}

/// `text` cut to `max` columns with the ellipsis in the middle and more of the end kept: items
/// that share a folder or a prefix differ at the end.
fn clip_middle(text: &str, max: usize) -> String {
    if text::width(text) <= max {
        return text.to_string();
    }
    let max = max.max(2);
    let tail_room = (max - 1) * 2 / 3;
    let head_room = max - 1 - tail_room;
    let mut head = String::new();
    for c in text.chars() {
        if text::width(&head) + text::width(c.encode_utf8(&mut [0; 4])) > head_room {
            break;
        }
        head.push(c);
    }
    let mut tail: Vec<char> = Vec::new();
    for c in text.chars().rev() {
        let used: usize = tail.iter().map(|c| text::width(c.encode_utf8(&mut [0; 4]))).sum();
        if used + text::width(c.encode_utf8(&mut [0; 4])) > tail_room {
            break;
        }
        tail.push(c);
    }
    format!("{head}…{}", tail.iter().rev().collect::<String>())
}

fn glyph(state: State, frame: usize) -> String {
    match state {
        State::Queued => format!("{FAINT}○{RESET}"),
        State::Running => format!("{LAGOON}{}{RESET}", QUARTER[frame % QUARTER.len()]),
        State::Done => format!("{FAINT}✓{RESET}"),
        State::Failed => format!("{DANGER}✗{RESET}"),
    }
}

fn member_row(member: &Member, width: usize, frame: usize, name: usize) -> String {
    let item = clip_middle(&member.item, name);
    let item = format!("{item}{}", " ".repeat(name.saturating_sub(text::width(&item))));
    let item = match member.state {
        State::Running | State::Failed => item,
        State::Queued | State::Done => format!("{DIM}{item}{RESET}"),
    };
    let stats = member.started.map_or(String::new(), |started| {
        let took = member.ended.unwrap_or_else(Instant::now).duration_since(started);
        format!("  {DIM}{:>5} {FAINT}·{RESET}{DIM} {:<8}{RESET}", clock(took), crate::tools::count(member.calls, "call"))
    });
    let doing = if member.state == State::Running && !member.doing.is_empty() { format!("  {DIM}{}{RESET}", member.doing) } else { String::new() };
    fit(&format!("  {} {item}{stats}{doing}", glyph(member.state, frame)), width)
}

/// The line a subagent leaves in the conversation when it ends.
pub fn finished_row(item: &str, failed: bool, calls: usize, took: Duration, width: usize) -> String {
    let glyph = glyph(if failed { State::Failed } else { State::Done }, 0);
    let note = format!(" {DIM}· {} · {}{RESET}", clock(took), crate::tools::count(calls, "call"));
    let note = if failed { format!(" {DANGER}failed{RESET}{note}") } else { note };
    let room = width.saturating_sub(text::width(&note) + 6).max(8);
    fit(&format!("  {DIM}↳{RESET} {glyph} {}{note}", clip_middle(item, room)), width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unstyled(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(char::is_ascii_alphabetic);
            } else {
                out.push(c);
            }
        }
        out
    }

    fn swarm(count: usize) -> Swarm {
        Swarm::new("call_1".into(), "Review PRs batch 1".into(), (1..=count).map(|n| format!("PR {}", 1500 + n)).collect())
    }

    #[test]
    fn header_counts_what_has_started_finished_and_failed() {
        let mut s = swarm(5);
        assert!(unstyled(&s.rows(100, 0, 12)[0]).contains("0/5 done · 5 queued"));
        s.call(0, "bash cargo test");
        s.call(1, "read src/lib.rs");
        s.call(2, "grep -n x");
        s.finish(0, false);
        s.finish(1, true);
        let header = unstyled(&s.rows(100, 0, 12)[0]);
        assert!(header.contains("2/5 done · 1 running · 2 queued · 1 failed"), "{header}");
        assert!(header.contains("swarm Review PRs batch 1"), "{header}");
        let narrower = unstyled(&s.rows(80, 0, 12)[0]);
        assert!(narrower.contains("swarm Review PRs batch 1") && narrower.contains("2/5 done · 1 running · 1 failed"), "{narrower}");
    }

    #[test]
    fn every_subagent_has_a_row_with_its_state_and_work() {
        let mut s = swarm(4);
        s.call(0, "bash cargo test");
        s.call(0, "bash npm run build");
        s.call(1, "read src/lib.rs");
        s.finish(1, false);
        let rows: Vec<String> = s.rows(80, 0, 12).iter().map(|r| unstyled(r)).collect();
        assert_eq!(rows.len(), 5);
        assert!(rows[1].starts_with("  ◐ PR 1501") && rows[1].contains("2 calls") && rows[1].ends_with("$ npm run build"), "{}", rows[1]);
        assert!(rows[2].starts_with("  ✓ PR 1502") && rows[2].contains("1 call") && !rows[2].contains("read"), "{}", rows[2]);
        assert!(rows[3].starts_with("  ○ PR 1503") && !rows[3].contains("call"), "{}", rows[3]);
    }

    #[test]
    fn a_big_swarm_is_a_strip_and_the_working_rows_within_the_budget() {
        let mut s = swarm(128);
        for n in 0..20 {
            s.call(n, "bash make");
            if n < 6 {
                s.finish(n, false);
            }
        }
        for width in [118, 80, 60] {
            let rows = s.rows(width, 1, 12);
            assert!(rows.len() <= 12, "{width}: {} rows", rows.len());
            assert!(rows.iter().all(|r| text::width(r) <= width), "{width}");
            let strip: usize = rows[1..].iter().map(|r| unstyled(r)).take_while(|r| !r.contains(" call")).map(|r| r.trim().chars().count()).sum();
            assert_eq!(strip, 128.min((width - 4) * STRIP_ROWS), "{width}");
            let last = unstyled(rows.last().unwrap());
            assert!(last.contains("more working"), "{width}: {last}");
        }
    }

    #[test]
    fn the_header_keeps_its_count_in_a_narrow_window() {
        let mut s = swarm(30);
        s.call(0, "bash make");
        s.finish(0, true);
        let header = unstyled(&s.rows(60, 0, 8)[0]);
        assert!(text::width(&header) <= 60 && header.contains("1/30 done"), "{header}");
    }

    #[test]
    fn a_long_name_keeps_its_end() {
        assert_eq!(clip_middle("packages/service-09", 19), "packages/service-09");
        let cut = clip_middle("packages/service-09", 12);
        assert!(text::width(&cut) <= 12 && cut.contains('…') && cut.ends_with("e-09"), "{cut}");
        assert!(text::width(&clip_middle("日本語のとても長い名前です", 9)) <= 9);
    }

    #[test]
    fn a_subagent_leaves_one_line_when_it_ends() {
        let row = unstyled(&finished_row("PR 1581", false, 12, Duration::from_secs(182), 80));
        assert_eq!(row, "  ↳ ✓ PR 1581 · 3m02s · 12 calls");
        let row = unstyled(&finished_row("PR 1594", true, 1, Duration::from_secs(5), 80));
        assert_eq!(row, "  ↳ ✗ PR 1594 failed · 5s · 1 call");
    }
}
