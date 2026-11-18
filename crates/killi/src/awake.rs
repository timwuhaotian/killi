//! Keeps the Mac from idle-sleeping while a turn works. A request in flight when the system
//! sleeps dies with it, and the turn ends on a connection error until the user says
//! "continue". `caffeinate -i` holds the idle-sleep assertion only: the display may still
//! sleep, and closing the lid still sleeps the machine.

use std::process::{Child, Command, Stdio};

/// The assertion, held until this is dropped.
pub struct Awake(Option<Child>);

/// Holds the assertion for the caller's scope. Off macOS, under tests, or when `caffeinate`
/// can't start, it holds nothing; the turn runs the same either way.
pub fn hold() -> Awake {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return Awake(None);
    }
    Awake(spawn())
}

/// `caffeinate` watching this process with `-w`, so it exits with killi even when killi is
/// killed and `Drop` never runs.
fn spawn() -> Option<Child> {
    Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

impl Drop for Awake {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_is_inert_under_tests() {
        assert!(hold().0.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_assertion_lasts_until_drop() {
        let mut awake = Awake(spawn());
        let child = awake.0.as_mut().expect("caffeinate starts");
        assert!(child.try_wait().unwrap().is_none(), "held while in scope");
        let pid = child.id();
        drop(awake);
        let alive = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "released on drop");
    }
}
