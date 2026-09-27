//! The wire format: one JSON object per line, inside a versioned envelope.

use super::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Request, Response};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RequestEnvelope {
    pub protocol: u32,
    pub request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResponseEnvelope {
    pub protocol: u32,
    pub response: Response,
}

/// Just enough of any envelope, of any version, to find its kind.
#[derive(Deserialize)]
struct AnyEnvelope<T> {
    protocol: u32,
    request: T,
}

#[derive(Deserialize)]
struct Kind {
    #[serde(rename = "type")]
    kind: String,
}

/// What the service makes of an incoming line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Incoming {
    Request(Request),
    /// Another protocol version, and not part of the stable handshake.
    OtherVersion,
    Malformed,
}

/// Decode a request line. `identify` and `shutdown` are accepted in any
/// protocol version (the stable handshake); anything else must match.
pub(crate) fn decode_request(line: &str) -> Incoming {
    if let Ok(envelope) = serde_json::from_str::<RequestEnvelope>(line)
        && envelope.protocol == PROTOCOL_VERSION
    {
        return Incoming::Request(envelope.request);
    }
    match serde_json::from_str::<AnyEnvelope<Kind>>(line) {
        Ok(any) if any.protocol == PROTOCOL_VERSION => Incoming::Malformed,
        Ok(any) => match any.request.kind.as_str() {
            "identify" => Incoming::Request(Request::Identify),
            "shutdown" => Incoming::Request(Request::Shutdown),
            _ => Incoming::OtherVersion,
        },
        Err(_) => Incoming::Malformed,
    }
}

/// Write `value` as one line. Refuses messages over the size limit.
pub(crate) fn write_message(stream: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() >= MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

/// Read one line, at most [`MAX_MESSAGE_BYTES`] long.
pub(crate) fn read_message(stream: &mut impl Read) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(stream.take(MAX_MESSAGE_BYTES as u64)).read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unterminated or oversized message",
        ));
    }
    Ok(line)
}

/// Decode a line leniently as `T`, ignoring fields it does not know.
pub(crate) fn decode<T: DeserializeOwned>(line: &str) -> Option<T> {
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(protocol: u32, request: &str) -> String {
        format!(r#"{{"protocol":{protocol},"request":{request}}}"#)
    }

    #[test]
    fn the_current_version_decodes_every_request() {
        assert_eq!(
            decode_request(&line(PROTOCOL_VERSION, r#"{"type":"snapshot","since":4}"#)),
            Incoming::Request(Request::Snapshot { since: Some(4) })
        );
        assert_eq!(
            decode_request(&line(PROTOCOL_VERSION, r#"{"type":"snapshot"}"#)),
            Incoming::Request(Request::Snapshot { since: None })
        );
    }

    #[test]
    fn only_the_stable_handshake_crosses_versions() {
        for version in [0, PROTOCOL_VERSION + 1, 99] {
            assert_eq!(
                decode_request(&line(version, r#"{"type":"identify"}"#)),
                Incoming::Request(Request::Identify)
            );
            assert_eq!(
                decode_request(&line(version, r#"{"type":"shutdown"}"#)),
                Incoming::Request(Request::Shutdown)
            );
            assert_eq!(
                decode_request(&line(version, r#"{"type":"logout"}"#)),
                Incoming::OtherVersion
            );
        }
    }

    #[test]
    fn garbage_is_malformed_not_a_crash() {
        for input in [
            "not json",
            "{}",
            r#"{"protocol":1}"#,
            &line(PROTOCOL_VERSION, r#"{"type":"fly"}"#),
            &line(
                PROTOCOL_VERSION,
                r#"{"type":"command","command":{"type":"set_volume","percent":"loud"}}"#,
            ),
        ] {
            assert_eq!(decode_request(input), Incoming::Malformed, "{input}");
        }
    }

    #[test]
    fn the_handshake_keeps_its_wire_shape() {
        // These lines must keep working in every future version.
        let identify = serde_json::to_string(&Request::Identify).unwrap();
        assert_eq!(identify, r#"{"type":"identify"}"#);
        let shutdown = serde_json::to_string(&Request::Shutdown).unwrap();
        assert_eq!(shutdown, r#"{"type":"shutdown"}"#);
    }

    #[test]
    fn oversized_and_unterminated_messages_are_refused() {
        let big = "x".repeat(MAX_MESSAGE_BYTES + 10);
        assert!(read_message(&mut big.as_bytes()).is_err());
        assert!(read_message(&mut "no newline".as_bytes()).is_err());
        assert_eq!(read_message(&mut "ok\nrest".as_bytes()).unwrap(), "ok\n");
    }
}
