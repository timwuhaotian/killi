//! The records a session's log gets when its process ends without the session ending it: a
//! panic, or a signal it quits on. They are written best effort, from wherever the process is
//! when it stops.

use std::io::Write;

/// The crash record in the session log's shape: one JSON object with a millisecond stamp.
pub(crate) fn crash_line(message: &str, ts: u128) -> String {
    format!(
        "{{\"type\":\"crash\",\"message\":{},\"ts\":{ts}}}\n",
        serde_json::Value::String(message.to_string())
    )
}

/// The record of quitting on `signal` (`SIGINT`, `SIGHUP`, `SIGTERM`), in the crash record's
/// shape.
pub(crate) fn signal_line(signal: &str, ts: u128) -> String {
    format!(
        "{{\"type\":\"signal\",\"signal\":{},\"ts\":{ts}}}\n",
        serde_json::Value::String(signal.to_string())
    )
}

/// Notes in the session log that the process quits on `signal`, so a log that simply stops
/// says why.
pub(crate) fn note_signal(signal: &str) {
    append_record(|ts| signal_line(signal, ts));
}

/// Appends the record `line` makes from a millisecond stamp to the session log, best effort:
/// nothing is left running to retry with. A crash may have torn the last line, so the record
/// starts on a fresh line instead of gluing onto it.
pub(crate) fn append_record(line: impl FnOnce(u128) -> String) {
    use std::io::{Read, Seek, SeekFrom};

    let Some(path) = crate::agent::crash_log() else {
        return;
    };
    let mut file = match std::fs::OpenOptions::new()
        .append(true)
        .read(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(_) => return,
    };
    let torn = match file.seek(SeekFrom::End(0)) {
        Ok(0) => false,
        Ok(_) => file
            .seek(SeekFrom::End(-1))
            .and_then(|_| {
                let mut last = [0u8; 1];
                file.read_exact(&mut last).map(|()| last[0] != b'\n')
            })
            .unwrap_or(false),
        Err(_) => false,
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut line = line(ts);
    if torn {
        line.insert(0, '\n');
    }
    let _ = file.write_all(line.as_bytes());
}
