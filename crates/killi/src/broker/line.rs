//! The JSON lines MCP servers, the broker and killi send each other, read with a cap so one
//! runaway line can't take all memory, and decoded leniently so one bad byte doesn't end the
//! stream.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// The longest line kept; a longer one is read through and dropped.
pub(crate) const MAX_LINE: usize = 32 * 1024 * 1024;

/// The next line without its line ending, or `None` at the end of the stream. Bytes that
/// aren't UTF-8 are replaced, and a line over `MAX_LINE` comes back empty, so a caller that
/// parses JSON skips both. Not cancel safe: dropped partway, the part already read is lost.
pub(crate) async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    read_capped(reader, MAX_LINE).await
}

pub(super) async fn read_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    let (mut read_any, mut over) = (false, false);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !read_any {
                return Ok(None);
            }
            break;
        }
        read_any = true;
        let (used, ends) = match available.iter().position(|&b| b == b'\n') {
            Some(newline) => (newline + 1, true),
            None => (available.len(), false),
        };
        if !over {
            if line.len() + used - usize::from(ends) > max {
                over = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(&available[..used]);
            }
        }
        reader.consume(used);
        if ends {
            break;
        }
    }
    if over {
        return Ok(Some(String::new()));
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    Ok(Some(String::from_utf8(line).unwrap_or_else(|e| {
        String::from_utf8_lossy(e.as_bytes()).into_owned()
    })))
}
