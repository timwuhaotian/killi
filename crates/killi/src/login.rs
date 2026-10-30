//! `killi login` and `killi logout`: the services killi signs in to, named on the command line
//! or picked from a list. ChatGPT signs in through the browser; OpenCode Go takes the API key
//! from its console.

pub mod chatgpt;
mod opencode_go;

pub(crate) use opencode_go::{add as opencode_go_add, signed_in as opencode_go_signed_in};

use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

/// A service `killi login` signs in to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Service {
    ChatGpt,
    OpencodeGo,
}

impl Service {
    const ALL: [Service; 2] = [Service::ChatGpt, Service::OpencodeGo];

    fn name(self) -> &'static str {
        match self {
            Service::ChatGpt => "chatgpt",
            Service::OpencodeGo => "opencode-go",
        }
    }

    /// Its line in the list `killi login` shows.
    fn about(self) -> &'static str {
        match self {
            Service::ChatGpt => "ChatGPT: your Plus or Pro plan, signed in in the browser",
            Service::OpencodeGo => {
                "OpenCode Go: your subscription, with the API key from the OpenCode Console"
            }
        }
    }

    fn signed_in(self) -> bool {
        match self {
            Service::ChatGpt => chatgpt::signed_in(),
            Service::OpencodeGo => opencode_go::signed_in(),
        }
    }
}

/// `killi login [name]`: signs in to the named service, or to one picked from the list.
pub async fn login(name: Option<&str>) -> Result<()> {
    match pick(name, &Service::ALL, "Sign in to")? {
        Service::ChatGpt => chatgpt::login().await,
        Service::OpencodeGo => opencode_go::login().await,
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
    }
}

/// The service `name` names, or the one picked from a numbered list of `listed`, where Enter
/// takes the first. Without a terminal to ask on, the name has to be given.
fn pick(name: Option<&str>, listed: &[Service], question: &str) -> Result<Service> {
    let names = Service::ALL.map(Service::name).join(", ");
    if let Some(name) = name {
        return choose(name, &Service::ALL)
            .ok_or_else(|| anyhow!("killi has no sign-in for `{name}`; it has: {names}"));
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("name the service, one of: {names}");
    }
    eprintln!("{question}:");
    for (number, service) in listed.iter().enumerate() {
        eprintln!("  {}. {}", number + 1, service.about());
    }
    let answer = crate::config::read_line("Which? [1] ")?;
    choose(&answer, listed).ok_or_else(|| anyhow!("`{answer}` isn't one of them"))
}

/// The service an answer picks from `listed`: the first for no answer, else by number or name.
fn choose(answer: &str, listed: &[Service]) -> Option<Service> {
    let answer = answer.trim();
    if answer.is_empty() {
        return listed.first().copied();
    }
    if let Ok(number) = answer.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|index| listed.get(index))
            .copied();
    }
    listed
        .iter()
        .copied()
        .find(|service| service.name().eq_ignore_ascii_case(answer))
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
    fn an_answer_picks_by_number_or_name_and_enter_takes_the_first() {
        assert_eq!(choose("", &Service::ALL), Some(Service::ChatGpt));
        assert_eq!(choose("2", &Service::ALL), Some(Service::OpencodeGo));
        assert_eq!(
            choose(" OpenCode-Go ", &Service::ALL),
            Some(Service::OpencodeGo)
        );
        assert_eq!(choose("3", &Service::ALL), None);
        assert_eq!(choose("0", &Service::ALL), None);
        assert_eq!(choose("zen", &Service::ALL), None);
        let one = [Service::OpencodeGo];
        assert_eq!(
            choose("", &one),
            Some(Service::OpencodeGo),
            "Enter takes the only one listed"
        );
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
        assert!(error.contains("chatgpt, opencode-go"), "{error}");
    }
}
