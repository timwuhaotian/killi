//! `killi login` and `killi logout`: the services killi signs in to, named on the command line
//! or picked from a list. ChatGPT, Command Code and Grok sign in through the browser; OpenCode
//! Go takes the API key from its console, and Muse Code the muse CLI's. Gemini has no sign-in
//! here: Google allows third-party agents only an API key (`/provider gemini`).

pub mod chatgpt;
mod commandcode;
pub mod grok;
mod muse_code;
mod opencode_go;

pub(crate) use commandcode::{
    add as commandcode_add, offer as commandcode_offer, signed_in as commandcode_signed_in,
};
pub(crate) use opencode_go::{
    add as opencode_go_add, add_env as opencode_go_add_env, signed_in as opencode_go_signed_in,
};

use std::fs;
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

/// A service `killi login` signs in to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Service {
    ChatGpt,
    OpencodeGo,
    MuseCode,
    CommandCode,
    Grok,
}

impl Service {
    const ALL: [Service; 5] = [
        Service::ChatGpt,
        Service::OpencodeGo,
        Service::MuseCode,
        Service::CommandCode,
        Service::Grok,
    ];

    fn name(self) -> &'static str {
        match self {
            Service::ChatGpt => "chatgpt",
            Service::OpencodeGo => "opencode-go",
            Service::MuseCode => "muse-code",
            Service::CommandCode => "command-code",
            Service::Grok => "grok",
        }
    }

    /// Its line in the list `killi login` shows.
    fn about(self) -> &'static str {
        match self {
            Service::ChatGpt => "ChatGPT: your Plus or Pro plan, signed in in the browser",
            Service::OpencodeGo => {
                "OpenCode Go: your subscription, with the API key from the OpenCode Console"
            }
            Service::MuseCode => {
                "Muse Code: Meta's subscription, with the API key from the muse CLI"
            }
            Service::CommandCode => {
                "Command Code: your subscription, signed in with commandcode.ai in the browser"
            }
            Service::Grok => "Grok: your SuperGrok or X Premium plan, signed in in the browser",
        }
    }

    fn signed_in(self) -> bool {
        match self {
            Service::ChatGpt => chatgpt::signed_in(),
            Service::OpencodeGo => opencode_go::signed_in(),
            Service::MuseCode => muse_code::signed_in(),
            Service::CommandCode => commandcode::signed_in(),
            Service::Grok => grok::signed_in(),
        }
    }
}

/// `killi login [name]`: signs in to the named service, or to one picked from the list.
pub async fn login(name: Option<&str>) -> Result<()> {
    match pick(name, &Service::ALL, "Sign in to")? {
        Service::ChatGpt => chatgpt::login().await,
        Service::OpencodeGo => opencode_go::login().await,
        Service::MuseCode => muse_code::login(),
        Service::CommandCode => commandcode::login().await,
        Service::Grok => grok::login().await,
    }
}

/// `killi logout [name]`: signs out of the named service, or of one picked from those killi is
/// signed in to.
pub async fn logout(name: Option<&str>) -> Result<()> {
    let signed: Vec<Service> = Service::ALL
        .into_iter()
        .filter(|service| service.signed_in())
        .collect();
    if name.is_none() && signed.is_empty() {
        println!("killi isn't signed in to anything.");
        return Ok(());
    }
    match pick(name, &signed, "Sign out of")? {
        Service::ChatGpt => chatgpt::logout().await,
        Service::OpencodeGo => opencode_go::logout(),
        Service::MuseCode => muse_code::logout(),
        Service::CommandCode => commandcode::logout(),
        Service::Grok => grok::logout().await,
    }
}

/// The service `name` names, or the one picked with the arrow keys from `listed`. Without a
/// terminal to ask on, the name has to be given.
fn pick(name: Option<&str>, listed: &[Service], question: &str) -> Result<Service> {
    let names = Service::ALL.map(Service::name).join(", ");
    if let Some(name) = name {
        return choose(name, &Service::ALL)
            .ok_or_else(|| anyhow!("killi has no sign-in for `{name}`; it has: {names}"));
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("name the service, one of: {names}");
    }
    if listed.is_empty() {
        bail!("no services to pick from");
    }
    let options: Vec<String> = listed
        .iter()
        .map(|service| service.about().to_string())
        .collect();
    let chosen = listed[crate::config::select(question, &options, 0)?];
    eprintln!(
        "  \x1b[32m✓\x1b[0m {question} \x1b[1m{}\x1b[0m\n",
        chosen.name()
    );
    Ok(chosen)
}

/// The service in `listed` with this name, case-insensitively.
fn choose(answer: &str, listed: &[Service]) -> Option<Service> {
    let answer = answer.trim();
    listed
        .iter()
        .copied()
        .find(|service| service.name().eq_ignore_ascii_case(answer))
}

/// Opens `url` in the system browser; the printed link is there when it can't.
pub(crate) fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Why a request on a plan has no token: signing in again fixes `SignIn`, waiting may fix
/// `Transient`.
#[derive(Debug)]
pub enum TokenError {
    SignIn(String),
    Transient(String),
}

/// An exclusive `flock` on a file beside a plan's credentials, held while one killi changes
/// them: refresh tokens rotate, so one killi refreshes at a time and the others take its result.
pub(crate) struct Lock {
    _file: fs::File,
}

impl Lock {
    /// Waits up to 30 s for the lock beside `credentials`; `plan` names it in the error.
    pub(crate) async fn acquire(credentials: &Path, plan: &str) -> std::io::Result<Self> {
        // A first sign-in locks before anything has made `~/.killi`.
        if let Some(dir) = credentials.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(credentials.with_extension("lock"))?;
        for _ in 0..600 {
            // SAFETY: the descriptor belongs to `file`, which stays open for the call.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { _file: file });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(std::io::Error::other(format!(
            "another killi kept the {plan} sign-in locked for 30 s"
        )))
    }
}

fn http_client() -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder().user_agent(crate::llm::USER_AGENT);
    Ok(builder
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_picks_a_service_case_insensitively() {
        assert_eq!(
            choose(" OpenCode-Go ", &Service::ALL),
            Some(Service::OpencodeGo)
        );
        assert_eq!(choose("muse-code", &Service::ALL), Some(Service::MuseCode));
        assert_eq!(
            choose("Command-Code", &Service::ALL),
            Some(Service::CommandCode)
        );
        assert_eq!(choose("2", &Service::ALL), None);
        assert_eq!(choose("zen", &Service::ALL), None);
        let one = [Service::OpencodeGo];
        assert_eq!(
            choose("chatgpt", &one),
            None,
            "a name must be on the list shown"
        );
        assert_eq!(choose("", &[]), None);
    }

    #[test]
    fn a_named_service_needs_no_list() {
        assert_eq!(pick(Some("chatgpt"), &[], "").unwrap(), Service::ChatGpt);
        let error = pick(Some("zen"), &Service::ALL, "")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("chatgpt, opencode-go, muse-code, command-code, grok"),
            "{error}"
        );
    }
}
