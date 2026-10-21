/// A test-run report, sent to the dashboard as JSON.
#[derive(Debug, Clone)]
pub struct Report {
    pub suite: String,
    pub passed: u32,
    pub failed: u32,
    pub duration_ms: u64,
    pub flaky: bool,
    /// Names of the failing tests, in order.
    pub failures: Vec<String>,
    /// Optional free-form note from whoever ran the suite.
    pub note: Option<String>,
}

fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Report {
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"suite\":");
        string(&mut out, &self.suite);
        out.push_str(&format!(",\"passed\":{},\"failed\":{},\"duration_ms\":{},\"flaky\":{},\"failures\":[", self.passed, self.failed, self.duration_ms, self.flaky));
        for (i, f) in self.failures.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            string(&mut out, f);
        }
        out.push_str("],\"note\":");
        match &self.note {
            Some(note) => string(&mut out, note),
            None => out.push_str("null"),
        }
        out.push('}');
        out
    }
}
