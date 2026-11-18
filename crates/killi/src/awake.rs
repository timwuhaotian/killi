//! Keeps the Mac from idle-sleeping while a turn works. A request in flight when the system
//! sleeps dies with it, and the turn ends on a connection error until the user says
//! "continue". `caffeinate -i` holds the idle-sleep assertion only: the display may still
//! sleep, and closing the lid still sleeps the machine. A question waiting on the user
//! pauses it, so a turn left asking doesn't keep the machine up for hours.

use std::cell::RefCell;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

/// A session's assertion, shared by its subagents: held while any `Hold` lives and no
/// `Pause` does.
#[derive(Default, Clone)]
pub struct Awake(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    holds: u32,
    pauses: u32,
    child: Option<Child>,
}

/// A turn's claim on the assertion, released on drop, however the turn ends.
pub struct Hold(Rc<RefCell<State>>);

/// Lets the machine sleep while the user is being waited on; the hold resumes on drop.
pub struct Pause(Rc<RefCell<State>>);

impl Awake {
    pub fn hold(&self) -> Hold {
        self.0.borrow_mut().holds += 1;
        refresh(&self.0);
        Hold(self.0.clone())
    }

    pub fn pause(&self) -> Pause {
        self.0.borrow_mut().pauses += 1;
        refresh(&self.0);
        Pause(self.0.clone())
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.borrow_mut().holds -= 1;
        refresh(&self.0);
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        self.0.borrow_mut().pauses -= 1;
        refresh(&self.0);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        release(&mut self.child);
    }
}

/// Starts or stops `caffeinate` to match the counts.
fn refresh(state: &RefCell<State>) {
    let mut state = state.borrow_mut();
    let wanted = state.holds > 0 && state.pauses == 0;
    if wanted && state.child.is_none() {
        state.child = spawn();
    } else if !wanted {
        release(&mut state.child);
    }
}

fn release(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// `caffeinate` watching this process with `-w`, so it exits with killi even when killi is
/// killed and no `Drop` runs. Nothing off macOS or under tests, or when it can't start; the
/// turn runs the same either way.
fn spawn() -> Option<Child> {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return None;
    }
    start()
}

fn start() -> Option<Child> {
    Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawns_nothing_under_tests() {
        let awake = Awake::default();
        let _hold = awake.hold();
        assert!(awake.0.borrow().child.is_none());
    }

    #[test]
    fn a_pause_lifts_the_hold_until_it_drops() {
        let awake = Awake::default();
        let wanted = |awake: &Awake| {
            let state = awake.0.borrow();
            state.holds > 0 && state.pauses == 0
        };
        let hold = awake.hold();
        assert!(wanted(&awake));
        let pause = awake.pause();
        assert!(!wanted(&awake));
        drop(pause);
        assert!(wanted(&awake));
        drop(hold);
        assert!(!wanted(&awake));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn caffeinate_lasts_until_released() {
        let mut child = start();
        let pid = child.as_ref().expect("caffeinate starts").id();
        assert!(child.as_mut().unwrap().try_wait().unwrap().is_none());
        release(&mut child);
        let alive = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "released");
    }
}
