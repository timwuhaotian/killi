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
