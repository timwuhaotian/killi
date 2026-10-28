mod browser;
mod fetch;
mod readable;
mod search;
pub(crate) mod setup;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use dom_smoothie::{Config, Readability, TextMode};
use regex::Regex;
use serde_json::Value;
use tokio::process::Command;

use super::{Context, MAX_BYTES, Output, SearchSettings, files, spill, uint};
use super::bash::{OwnSession, Tracked, track};
use crate::llm::truncate;

pub use fetch::fetch_url;
pub use search::web_search;
pub use setup::{CONTAINER, start_searxng};

use browser::{Temp, render, screenshot};
use readable::{decode, extract, html_title, plain};

#[cfg(test)]
use browser::Running;
#[cfg(test)]
use fetch::{fetch_error, raw_github, shown};
#[cfg(test)]
use readable::readable;
#[cfg(test)]
use search::{Source, brave_page, hits, retryable, search, search_chain, sources};

const MAX_DOWNLOAD: usize = 10 * 1024 * 1024;
/// Readable text shorter than this suggests a page built by JavaScript.
const MIN_READABLE: usize = 200;
/// Some sites turn away clients that don't look like a browser.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36";

#[cfg(test)]
mod tests;
