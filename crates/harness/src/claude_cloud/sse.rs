//! A minimal Server-Sent Events decoder for the session stream: blocks
//! separated by a blank line, `event:` / `id:` / `data:` fields, `:` comments
//! (the server's keepalives) skipped.

/// One SSE event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` (`client_event`, `ephemeral_event`, `session_update`, …).
    pub event: String,
    /// `id:` (a stored event's sequence number).
    pub id: Option<String>,
    /// The `data:` lines, joined with `\n`.
    pub data: String,
}

/// Incremental decoder: feed it bytes as they arrive, take whole events out.
#[derive(Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    /// Feed `chunk`; returns every event it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((end, skip)) = block_end(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..end + skip).take(end).collect();
            if let Some(event) = parse_block(&String::from_utf8_lossy(&block)) {
                events.push(event);
            }
        }
        events
    }
}

/// Where the first complete block ends, and the separator's length.
fn block_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buffer.len() {
        for sep in [&b"\r\n\r\n"[..], b"\n\n", b"\r\r"] {
            if buffer[i..].starts_with(sep) {
                return Some((i, sep.len()));
            }
        }
        i += 1;
    }
    None
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = SseEvent::default();
    let mut data: Vec<&str> = Vec::new();
    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => event.event = value.to_owned(),
            "id" => event.id = Some(value.to_owned()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if data.is_empty() && event.event.is_empty() {
        return None;
    }
    if event.event.is_empty() {
        event.event = "message".into();
    }
    event.data = data.join("\n");
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_events_split_across_chunks_and_skips_comments() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(b": keepalive\n\nevent: client_event\nid: 7\nda")
                .is_empty()
        );
        let events = decoder.push(b"ta: {\"a\":1}\n\nevent: ephemeral_event\r\ndata: {}\r\n\r\n");
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: "client_event".into(),
                    id: Some("7".into()),
                    data: "{\"a\":1}".into(),
                },
                SseEvent {
                    event: "ephemeral_event".into(),
                    id: None,
                    data: "{}".into(),
                },
            ]
        );
    }
}
