//! Server-sent events parsing (the transport half of TP-ADP-01 and TP-ADP-04). The parser must give
//! the same events however the network splits the bytes, and refuse input it can't trust.

use corral_adapters::sse::{SseError, SseEvent, SseParser};

fn parse_all(input: &[u8], chunk: usize) -> Result<Vec<SseEvent>, SseError> {
    let mut p = SseParser::default();
    let mut out = Vec::new();
    for piece in input.chunks(chunk.max(1)) {
        out.extend(p.push(piece)?);
    }
    Ok(out)
}

fn ev(event: Option<&str>, data: &str) -> SseEvent {
    SseEvent {
        event: event.map(str::to_owned),
        data: data.to_owned(),
    }
}

#[test]
fn events_are_the_same_whatever_the_chunking() {
    let input = b"event: a\ndata: {\"x\":1}\n\n: keep-alive comment\n\ndata: second\n\n";
    let expected = vec![ev(Some("a"), "{\"x\":1}"), ev(None, "second")];
    for chunk in 1..=input.len() {
        assert_eq!(
            parse_all(input, chunk).unwrap(),
            expected,
            "chunk size {chunk}"
        );
    }
}

#[test]
fn crlf_line_endings_and_multi_line_data() {
    let input = b"event: b\r\ndata: one\r\ndata: two\r\n\r\n";
    assert_eq!(
        parse_all(input, 3).unwrap(),
        vec![ev(Some("b"), "one\ntwo")]
    );
}

#[test]
fn a_field_without_a_space_after_the_colon_and_unknown_fields() {
    let input = b"event:c\nid: 7\nretry: 100\ndata:x\n\n";
    assert_eq!(parse_all(input, 5).unwrap(), vec![ev(Some("c"), "x")]);
}

#[test]
fn a_block_without_data_is_not_an_event() {
    let input = b"event: lonely\n\n";
    assert!(parse_all(input, 4).unwrap().is_empty());
}

#[test]
fn an_unfinished_event_at_the_end_is_held_back() {
    let mut p = SseParser::default();
    assert!(p.push(b"data: partial").unwrap().is_empty());
    assert!(p.push(b"\n").unwrap().is_empty());
    assert_eq!(p.push(b"\n").unwrap(), vec![ev(None, "partial")]);
}

#[test]
fn an_oversized_event_is_refused() {
    let mut p = SseParser::default();
    let big = vec![b'a'; SseParser::MAX_EVENT_BYTES + 1];
    let mut result = p.push(b"data: ");
    for piece in big.chunks(4096) {
        if result.is_err() {
            break;
        }
        result = p.push(piece);
    }
    assert_eq!(result.unwrap_err(), SseError::TooLarge);
}

#[test]
fn invalid_utf8_is_refused() {
    assert_eq!(
        parse_all(b"data: \xff\xfe\n\n", 64).unwrap_err(),
        SseError::InvalidUtf8
    );
}
