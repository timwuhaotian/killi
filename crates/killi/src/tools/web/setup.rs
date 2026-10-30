//! Starting the local SearXNG instance `/web-search-config` offers. Docker does the heavy
//! lifting — the image, the server, the restart on boot — and killi only runs it and waits,
//! so nothing here adds to the binary or the dependencies: the settings are written under
//! `~/.killi`, the container answers on localhost only, and the search stays local.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use ring::rand::SecureRandom;
use serde_json::Value;
use tokio::process::Command;

use crate::tools::OwnSession;

/// The container killi starts, the image it runs, and the localhost port it answers on.
pub const CONTAINER: &str = "killi-searxng";
const IMAGE: &str = "searxng/searxng";
const PORT: u16 = 8888;

/// How long the whole start may take: the first run pulls the image, a few hundred MB.
pub const START_TIMEOUT: Duration = Duration::from_secs(8 * 60);
/// How often the instance is probed while it starts.
const PROBE_EVERY: Duration = Duration::from_secs(1);

/// The instance's URL, also what `/web-search-config` writes into the config.
pub fn url() -> String {
    format!("http://localhost:{PORT}")
}

/// Where killi keeps the settings it mounts into the container.
pub fn settings_path() -> PathBuf {
    crate::config::killi_home()
        .join("searxng")
        .join("settings.yml")
}

/// The settings the instance runs with: the image's defaults, the json format `web_search`
/// reads, and no limiter, which would turn scripted requests away.
pub fn settings_yaml(secret: &str) -> String {
    format!(
        "use_default_settings: true\nserver:\n  base_url: {url}\n  secret_key: \"{secret}\"\n  limiter: false\nsearch:\n  formats:\n    - html\n    - json\n",
        url = url()
    )
}

/// A fresh 32-hex-character session secret.
fn secret() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "the system's random source failed".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The `docker run` that starts the container: named, restarted with Docker, on localhost
/// only, with killi's settings mounted read-only.
pub fn run_args(settings: &Path) -> Vec<String> {
    [
        "run",
        "-d",
        "--name",
        CONTAINER,
        "--restart",
        "unless-stopped",
        "-p",
        &format!("127.0.0.1:{PORT}:8080"),
        "-v",
        &format!("{}:/etc/searxng/settings.yml:ro", settings.display()),
        IMAGE,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Runs `docker` with `args`, in a session of its own and with nothing on the terminal, and
/// returns its stdout. A missing docker, a daemon that is down, or a failure each names
/// itself, in the words the next message can pass on.
async fn docker(args: &[&str]) -> Result<String, String> {
    let run = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .own_session()
        .output()
        .await;
    match run {
        Ok(out) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => {
            let said = String::from_utf8_lossy(&out.stderr)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let said = crate::llm::truncate(&said, 300);
            if said.contains("Cannot connect") {
                Err("Docker isn't running; start it and pick SearXNG again.".to_string())
            } else {
                Err(format!("`docker {}` failed: {said}", args.join(" ")))
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Err(
            "Docker isn't installed; get it from docker.com, or use an instance you run.".into(),
        ),
        Err(e) => Err(format!("could not run docker: {e}")),
    }
}

/// Starts the local instance and waits until its json API answers; the URL it answers on.
/// An instance already answering on the port is used as it is, and a container from an
/// earlier run is restarted, so fresh settings are read.
pub async fn start_searxng() -> Result<String, String> {
    let everything = async {
        let base = url();
        if wait_ready(&base, PROBE_EVERY, Duration::from_secs(2))
            .await
            .is_ok()
        {
            return Ok(base);
        }
        docker(&["version", "--format", "{{.Server.Version}}"]).await?;
        let path = settings_path();
        let yaml = settings_yaml(&secret()?);
        crate::fs_atomic::write(&path, yaml.as_bytes(), None)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        if docker(&["container", "inspect", CONTAINER]).await.is_ok() {
            docker(&["restart", CONTAINER])
                .await
                .map_err(|e| format!("could not restart the container: {e}"))?;
        } else {
            let args = run_args(&path);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            docker(&refs)
                .await
                .map_err(|e| format!("could not start the container: {e}"))?;
        }
        let base = url();
        wait_ready(&base, PROBE_EVERY, START_TIMEOUT).await?;
        Ok(base)
    };
    tokio::time::timeout(START_TIMEOUT, everything)
        .await
        .map_err(|_| format!("SearXNG did not answer within {} minutes; the first run pulls a large image, so pick it again once that is done.", START_TIMEOUT.as_secs() / 60))?
}

/// Probes `{base}/search?q=…&format=json` until a reply carries a results array, `per`
/// between tries and for at most `budget`. The same request `web_search` makes, so ready
/// means usable.
pub(super) async fn wait_ready(base: &str, per: Duration, budget: Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(per)
        .build()
        .map_err(|e| format!("could not create an HTTP client: {e}"))?;
    let deadline = Instant::now() + budget;
    loop {
        let probe = client
            .get(format!("{}/search", base.trim_end_matches('/')))
            .query(&[("q", "searxng"), ("format", "json")])
            .send()
            .await;
        if let Ok(reply) = probe
            && reply.status().is_success()
            && let Ok(body) = reply.text().await
            && let Ok(json) = serde_json::from_str::<Value>(&body)
            && json["results"].is_array()
        {
            return Ok(());
        }
        if Instant::now() + per >= deadline {
            return Err(format!("the instance never answered at {base}."));
        }
        tokio::time::sleep(per).await;
    }
}
