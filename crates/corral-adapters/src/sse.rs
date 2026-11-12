//! Server-sent events (the `text/event-stream` format), parsed incrementally.
//!
//! The parser gives the same events however the bytes are split, and refuses what it can't
//! trust: an event bigger than [`SseParser::MAX_EVENT_BYTES`], or text that isn't UTF-8.
//! Comments (`:`), `id` and `retry` are ignored; a block without `data` isn't an event.

/// One event: its `event:` name, if any, and its `data:` lines joined with `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field.
    pub event: Option<String>,
    /// The `data:` field.
    pub data: String,
}

/// Why a stream was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    /// One event grew past [`SseParser::MAX_EVENT_BYTES`].
    #[error("an event was too large")]
    TooLarge,
    /// The stream wasn't UTF-8.
    #[error("the stream isn't valid UTF-8")]
    InvalidUtf8,
}

/// Turns chunks of bytes into [`SseEvent`]s.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes of the current, unfinished line.
    line: Vec<u8>,
    /// Bytes buffered for the current event (lines so far plus the current line).
    pending: usize,
    event: Option<String>,
    data: Option<String>,
}

impl SseParser {
    /// The largest event accepted, in bytes.
    pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

    /// Feeds the next chunk; returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut out = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let line = String::from_utf8(line).map_err(|_| SseError::InvalidUtf8)?;
                if let Some(event) = self.line_done(&line) {
                    out.push(event);
                }
            } else {
                self.line.push(byte);
                self.pending += 1;
                if self.pending > Self::MAX_EVENT_BYTES {
                    return Err(SseError::TooLarge);
                }
            }
        }
        Ok(out)
    }

    fn line_done(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            self.pending = 0;
            let event = self.event.take();
            return self.data.take().map(|data| SseEvent { event, data });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
        None
    }
}
