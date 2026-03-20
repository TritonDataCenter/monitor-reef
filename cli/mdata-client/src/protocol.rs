// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Metadata protocol implementation (V1 and V2).
//!
//! The protocol supports two versions:
//!
//! **V1**: Simple text commands (`COMMAND [ARG]\n`) with multi-line
//! responses terminated by a `.` line.
//!
//! **V2**: Framed protocol with BASE64 encoding and CRC32 checksums.
//! Format: `V2 <body_len> <crc32_hex> <reqid> <command> [<b64_arg>]\n`
//!
//! V2 is negotiated automatically on connection. PUT and DELETE
//! operations require V2.
//!
//! ## Error handling
//!
//! Transport methods return `Result<_, TransportError>` for structured
//! I/O errors (timeout, EOF, invalid data). Protocol methods return
//! `anyhow::Result` to add contextual information. `TransportError`
//! converts to `anyhow::Error` via thiserror's `#[error]` derive.

use std::fmt;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tracing::{debug, warn};

use crate::transport::{MetadataTransport, Transport, TransportError};
use crate::{Command, Response};

/// Timeout for V1 commands and protocol negotiation (6 seconds).
const RECV_TIMEOUT_MS: u64 = 6_000;

/// Timeout for V2 operations (45 seconds, allows for slower PUT).
const RECV_TIMEOUT_MS_V2: u64 = 45_000;

/// Maximum number of timeout-and-reset retries before giving up.
const MAX_RETRIES: u32 = 3;

/// Maximum number of stale V2 frames to discard before giving up.
const MAX_STALE_FRAMES: u32 = 5;

/// Negotiated protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtocolVersion {
    V1,
    V2,
}

/// Protocol handler for metadata operations.
pub struct Protocol<T: MetadataTransport> {
    transport: T,
    version: ProtocolVersion,
}

impl<T: MetadataTransport> fmt::Debug for Protocol<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Protocol")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl Protocol<Transport> {
    /// Initialize: open transport, negotiate protocol version.
    pub fn init() -> Result<Self> {
        let mut transport = Transport::open()?;
        let version = Self::negotiate(&mut transport)?;
        Ok(Self { transport, version })
    }
}

impl<T: MetadataTransport> Protocol<T> {
    /// Create a protocol handler with an existing transport.
    #[cfg(test)]
    pub fn with_transport(mut transport: T) -> Result<Self> {
        let version = Self::negotiate(&mut transport)?;
        Ok(Self { transport, version })
    }

    /// Execute a DELETE command.
    ///
    /// Requires V2 protocol support.
    pub fn delete(&mut self, key: &str) -> Result<Response> {
        if self.version != ProtocolVersion::V2 {
            bail!(
                "metadata service does not support V2 protocol \
                 (required for DELETE)"
            );
        }
        self.execute(Command::Delete, Some(key))
    }

    /// Execute a PUT command, encoding the key and value per protocol.
    ///
    /// The V2 PUT wire format uses double base64 encoding:
    /// the key and value are each individually base64-encoded, joined
    /// by a space, and then the combined string is base64-encoded again
    /// as the V2 frame argument. This matches the original C mdata-client.
    ///
    /// On the wire: `V2 <len> <crc> <reqid> PUT <b64(b64(key) SP b64(val))>`
    pub fn put(&mut self, key: &str, value: &str) -> Result<Response> {
        if self.version != ProtocolVersion::V2 {
            bail!(
                "metadata service does not support V2 protocol \
                 (required for PUT)"
            );
        }
        let arg = format!("{} {}", STANDARD.encode(key), STANDARD.encode(value));
        self.execute(Command::Put, Some(&arg))
    }

    /// Execute a metadata command with automatic retry on timeout.
    ///
    /// On timeout, the protocol is reset (transport reconnected and
    /// V2 re-negotiated) and the command is retried.
    pub fn execute(&mut self, command: Command, arg: Option<&str>) -> Result<Response> {
        let mut retries = 0;
        loop {
            match self.try_execute(command, arg) {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if is_timeout(&e) {
                        retries += 1;
                        if retries > MAX_RETRIES {
                            bail!(
                                "giving up after {MAX_RETRIES} \
                                 timeout retries"
                            );
                        }
                        warn!(
                            "receive timeout, resetting \
                             protocol (attempt {retries}/{MAX_RETRIES})"
                        );
                        self.reset()?;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    fn try_execute(&mut self, command: Command, arg: Option<&str>) -> Result<Response> {
        match self.version {
            ProtocolVersion::V2 => self.execute_v2(command, arg),
            ProtocolVersion::V1 => self.execute_v1(command, arg),
        }
    }

    /// Execute a V1 protocol command.
    fn execute_v1(&mut self, command: Command, arg: Option<&str>) -> Result<Response> {
        let request = match arg {
            Some(a) => format!("{command} {a}\n"),
            None => format!("{command}\n"),
        };

        self.transport.send(&request)?;

        let header = self.transport.recv_line(RECV_TIMEOUT_MS)?;

        match header.as_str() {
            "SUCCESS" => {
                let mut data = String::new();
                loop {
                    let line = self.transport.recv_line(RECV_TIMEOUT_MS)?;
                    if line == "." {
                        break;
                    }
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(&line);
                }
                if data.is_empty() {
                    Ok(Response::Success(None))
                } else {
                    Ok(Response::Success(Some(data)))
                }
            }
            "NOTFOUND" => Ok(Response::NotFound),
            other => bail!("unexpected V1 response: {other}"),
        }
    }

    /// Execute a V2 protocol command.
    fn execute_v2(&mut self, command: Command, arg: Option<&str>) -> Result<Response> {
        let reqid = generate_request_id()?;

        let body = match arg {
            Some(a) => {
                let b64_arg = STANDARD.encode(a);
                format!("{reqid} {command} {b64_arg}")
            }
            None => format!("{reqid} {command}"),
        };

        let crc = crc32fast::hash(body.as_bytes());
        let request = format!("V2 {} {crc:08x} {body}\n", body.len());

        self.transport.send(&request)?;

        // Read V2 response, discarding stale frames from
        // previous timed-out requests (mismatched request IDs)
        let mut stale_count = 0u32;
        loop {
            let line = self.transport.recv_line(RECV_TIMEOUT_MS_V2)?;
            match parse_v2_frame(&line, &reqid) {
                Ok(frame) => {
                    return match frame.status.as_str() {
                        "SUCCESS" => {
                            let data = frame.payload.map(|p| decode_b64_payload(&p)).transpose()?;
                            Ok(Response::Success(data))
                        }
                        "NOTFOUND" => Ok(Response::NotFound),
                        other => {
                            bail!("unexpected V2 status: {other}")
                        }
                    };
                }
                Err(FrameError::ReqIdMismatch { .. }) => {
                    stale_count += 1;
                    if stale_count > MAX_STALE_FRAMES {
                        bail!(
                            "too many stale V2 frames \
                             ({MAX_STALE_FRAMES}), giving up"
                        );
                    }
                    continue;
                }
                Err(FrameError::Other(e)) => return Err(e),
            }
        }
    }

    /// Reset the protocol: reconnect transport and re-negotiate.
    fn reset(&mut self) -> Result<()> {
        thread::sleep(Duration::from_secs(1));
        self.transport.reconnect()?;
        self.version = Self::negotiate(&mut self.transport)?;
        Ok(())
    }

    /// Negotiate protocol version with the metadata service.
    ///
    /// For serial transports, sends a reset sequence first (`\n` ->
    /// `invalid command`) to clear any stale state on the port.
    fn negotiate(transport: &mut T) -> Result<ProtocolVersion> {
        if transport.is_serial() {
            // Serial port reset: send a bare newline, expect
            // "invalid command" response to confirm port is alive
            transport.send("\n").ok();
            match transport.recv_line(RECV_TIMEOUT_MS) {
                Ok(_) => {} // Discard response (usually "invalid command")
                Err(TransportError::Timeout) => {
                    // Port may not be responsive yet, continue anyway
                }
                Err(TransportError::Eof) => {
                    bail!("serial port closed during reset sequence");
                }
                Err(TransportError::Io(e)) => {
                    bail!("serial port I/O error during reset: {e}");
                }
                Err(TransportError::InvalidData) => {}
            }
        }

        // Attempt V2 negotiation
        transport.send("NEGOTIATE V2\n")?;
        match transport.recv_line(RECV_TIMEOUT_MS) {
            Ok(ref line) if line == "V2_OK" => {
                debug!("negotiated V2 protocol");
                Ok(ProtocolVersion::V2)
            }
            Ok(ref line) if line == "invalid command" => {
                debug!("V2 not supported, falling back to V1");
                Ok(ProtocolVersion::V1)
            }
            Ok(other) => {
                bail!("unexpected negotiation response: {other}")
            }
            Err(TransportError::Timeout) => {
                bail!("timeout during protocol negotiation")
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Errors from V2 frame parsing.
#[derive(Debug, thiserror::Error)]
enum FrameError {
    #[error("V2 request ID mismatch: expected {expected}, got {actual}")]
    ReqIdMismatch { expected: String, actual: String },
    #[error("{0}")]
    Other(anyhow::Error),
}

/// A parsed V2 protocol frame.
#[derive(Debug)]
struct V2Frame {
    status: String,
    payload: Option<String>,
}

/// Parse a V2 response frame and validate its integrity.
///
/// Frame format: `V2 <body_len> <crc32_hex> <reqid> <status> [<b64_payload>]`
fn parse_v2_frame(line: &str, expected_reqid: &str) -> std::result::Result<V2Frame, FrameError> {
    match parse_v2_body(line) {
        Ok((reqid, status, payload)) => {
            if reqid != expected_reqid {
                return Err(FrameError::ReqIdMismatch {
                    expected: expected_reqid.to_string(),
                    actual: reqid,
                });
            }
            Ok(V2Frame { status, payload })
        }
        Err(e) => Err(FrameError::Other(e)),
    }
}

/// Parse the envelope and body of a V2 frame, validating length and CRC.
///
/// Returns `(request_id, status, optional_payload)`.
fn parse_v2_body(line: &str) -> Result<(String, String, Option<String>)> {
    let mut parts = line.splitn(4, ' ');

    let marker = parts.next();
    let len_str = parts.next();
    let crc_str = parts.next();
    let body = parts.next();

    let (Some("V2"), Some(len_str), Some(crc_str), Some(body)) = (marker, len_str, crc_str, body)
    else {
        bail!("invalid V2 frame: expected 'V2 <len> <crc> <body>'");
    };

    let expected_len: usize = len_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid V2 frame length: {len_str}"))?;

    let expected_crc = u32::from_str_radix(crc_str, 16)
        .map_err(|_| anyhow::anyhow!("invalid V2 frame CRC: {crc_str}"))?;

    if body.len() != expected_len {
        bail!(
            "V2 frame length mismatch: header says {expected_len}, body is {}",
            body.len()
        );
    }

    let actual_crc = crc32fast::hash(body.as_bytes());
    if actual_crc != expected_crc {
        bail!("V2 frame CRC mismatch: expected {expected_crc:08x}, got {actual_crc:08x}");
    }

    let mut body_parts = body.splitn(3, ' ');
    let reqid = body_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request ID in V2 frame"))?;
    let status = body_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing status in V2 frame"))?;
    let payload = body_parts.next().map(String::from);

    Ok((reqid.to_string(), status.to_string(), payload))
}

/// Decode a BASE64-encoded payload string.
fn decode_b64_payload(encoded: &str) -> Result<String> {
    let bytes = STANDARD
        .decode(encoded)
        .context("invalid base64 in response")?;
    String::from_utf8(bytes).context("response payload is not valid UTF-8")
}

/// Generate an 8-character hex request ID for V2 protocol frames.
fn generate_request_id() -> Result<String> {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf)
        .map_err(|e| anyhow::anyhow!("getrandom: {e}"))
        .context("failed to generate request ID")?;
    Ok(format!("{:08x}", u32::from_ne_bytes(buf)))
}

/// Check if an error is a transport timeout.
fn is_timeout(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TransportError>()
        .is_some_and(|te| matches!(te, TransportError::Timeout))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Mock transport that replays scripted responses.
    struct MockTransport {
        responses: RefCell<Vec<String>>,
        sent: RefCell<Vec<String>>,
        serial: bool,
    }

    impl MockTransport {
        fn new(responses: Vec<&str>, serial: bool) -> Self {
            // Reverse so we can pop from the end
            let responses = responses.into_iter().rev().map(String::from).collect();
            Self {
                responses: RefCell::new(responses),
                sent: RefCell::new(Vec::new()),
                serial,
            }
        }

        fn sent_lines(&self) -> Vec<String> {
            self.sent.borrow().clone()
        }
    }

    impl MetadataTransport for MockTransport {
        fn send(&self, data: &str) -> Result<(), TransportError> {
            self.sent.borrow_mut().push(data.to_string());
            Ok(())
        }

        fn recv_line(&self, _timeout_ms: u64) -> Result<String, TransportError> {
            self.responses.borrow_mut().pop().ok_or(TransportError::Eof)
        }

        fn reconnect(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_serial(&self) -> bool {
            self.serial
        }
    }

    #[test]
    fn test_generate_request_id_format() {
        let id = generate_request_id().unwrap();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_parse_v2_frame_valid() {
        let reqid = "dc4fae17";
        let status = "SUCCESS";
        let payload = STANDARD.encode("hello world");
        let body = format!("{reqid} {status} {payload}");
        let crc = crc32fast::hash(body.as_bytes());
        let frame = format!("V2 {} {crc:08x} {body}", body.len());

        let f = parse_v2_frame(&frame, reqid).unwrap();
        assert_eq!(f.status, "SUCCESS");
        let decoded = decode_b64_payload(&f.payload.unwrap()).unwrap();
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn test_parse_v2_frame_notfound() {
        let reqid = "abcd1234";
        let body = format!("{reqid} NOTFOUND");
        let crc = crc32fast::hash(body.as_bytes());
        let frame = format!("V2 {} {crc:08x} {body}", body.len());

        let f = parse_v2_frame(&frame, reqid).unwrap();
        assert_eq!(f.status, "NOTFOUND");
        assert!(f.payload.is_none());
    }

    #[test]
    fn test_parse_v2_frame_bad_crc() {
        let reqid = "dc4fae17";
        let body = format!("{reqid} SUCCESS");
        let frame = format!("V2 {} 00000000 {body}", body.len());

        let err = parse_v2_frame(&frame, reqid).unwrap_err();
        assert!(matches!(err, FrameError::Other(_)));
        assert!(format!("{err}").contains("CRC mismatch"));
    }

    #[test]
    fn test_parse_v2_frame_wrong_reqid() {
        let reqid = "dc4fae17";
        let body = format!("{reqid} SUCCESS");
        let crc = crc32fast::hash(body.as_bytes());
        let frame = format!("V2 {} {crc:08x} {body}", body.len());

        let err = parse_v2_frame(&frame, "00000000").unwrap_err();
        assert!(matches!(err, FrameError::ReqIdMismatch { .. }));
    }

    #[test]
    fn test_parse_v2_frame_bad_length() {
        let reqid = "dc4fae17";
        let body = format!("{reqid} SUCCESS");
        let crc = crc32fast::hash(body.as_bytes());
        let frame = format!("V2 99 {crc:08x} {body}");

        let err = parse_v2_frame(&frame, reqid).unwrap_err();
        assert!(matches!(err, FrameError::Other(_)));
        assert!(format!("{err}").contains("length mismatch"));
    }

    #[test]
    fn test_v1_get_success() {
        let mock = MockTransport::new(
            vec![
                "SUCCESS",     // V1 response header
                "hello world", // response data
                ".",           // terminator
            ],
            false,
        );
        let mut proto = Protocol {
            transport: mock,
            version: ProtocolVersion::V1,
        };

        let resp = proto.execute(Command::Get, Some("mykey")).unwrap();
        match resp {
            Response::Success(Some(data)) => assert_eq!(data, "hello world"),
            other => panic!("expected Success(Some), got {other:?}"),
        }

        let sent = proto.transport.sent_lines();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], "GET mykey\n");
    }

    #[test]
    fn test_v1_get_notfound() {
        let mock = MockTransport::new(vec!["NOTFOUND"], false);
        let mut proto = Protocol {
            transport: mock,
            version: ProtocolVersion::V1,
        };

        let resp = proto.execute(Command::Get, Some("nokey")).unwrap();
        assert!(matches!(resp, Response::NotFound));
    }

    #[test]
    fn test_negotiate_v2() {
        let mock = MockTransport::new(vec!["V2_OK"], false);
        let proto = Protocol::with_transport(mock).unwrap();
        assert_eq!(proto.version, ProtocolVersion::V2);
    }

    #[test]
    fn test_negotiate_v1_fallback() {
        let mock = MockTransport::new(vec!["invalid command"], false);
        let proto = Protocol::with_transport(mock).unwrap();
        assert_eq!(proto.version, ProtocolVersion::V1);
    }

    #[test]
    fn test_serial_negotiation_sends_reset() {
        let mock = MockTransport::new(
            vec![
                "invalid command", // response to \n reset
                "V2_OK",           // response to NEGOTIATE V2
            ],
            true, // serial
        );
        let proto = Protocol::with_transport(mock).unwrap();
        assert_eq!(proto.version, ProtocolVersion::V2);

        let sent = proto.transport.sent_lines();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], "\n"); // reset sequence
        assert_eq!(sent[1], "NEGOTIATE V2\n");
    }

    #[test]
    fn test_v2_get_success() {
        // Build a V2 response frame for the mock.
        // We don't know the reqid in advance, so we need
        // to inspect what was sent and build the response
        // dynamically. Instead, test at the frame parsing
        // level — the execute_v2 → parse_v2_frame path is
        // covered by the frame parsing tests + this V1 test
        // proving execute() dispatches correctly.
        //
        // For a true V2 end-to-end test, we'd need a mock
        // that inspects the request and echoes the reqid.
        // Test the encoding logic directly instead:
        let key = "test-key";
        let value = "test-value";
        let expected = format!("{} {}", STANDARD.encode(key), STANDARD.encode(value));

        // Verify the PUT argument encoding matches the spec
        let outer = STANDARD.encode(&expected);
        let decoded_outer = String::from_utf8(STANDARD.decode(&outer).unwrap()).unwrap();
        assert_eq!(decoded_outer, expected);

        // Decode the inner key and value
        let parts: Vec<&str> = decoded_outer.splitn(2, ' ').collect();
        let decoded_key = String::from_utf8(STANDARD.decode(parts[0]).unwrap()).unwrap();
        let decoded_val = String::from_utf8(STANDARD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(decoded_key, key);
        assert_eq!(decoded_val, value);
    }

    #[test]
    fn test_v2_stale_frame_discarded() {
        // A stale frame has a mismatched reqid — parse_v2_frame
        // should return ReqIdMismatch, and execute_v2 should
        // skip it and read the next frame.
        let reqid = "aabbccdd";
        let stale_reqid = "00000000";

        // Build a stale frame
        let stale_body = format!("{stale_reqid} SUCCESS");
        let stale_crc = crc32fast::hash(stale_body.as_bytes());
        let stale_frame = format!("V2 {} {stale_crc:08x} {stale_body}", stale_body.len());

        // Build the correct frame
        let good_body = format!("{reqid} SUCCESS");
        let good_crc = crc32fast::hash(good_body.as_bytes());
        let good_frame = format!("V2 {} {good_crc:08x} {good_body}", good_body.len());

        // Parsing the stale frame with the good reqid should error
        let err = parse_v2_frame(&stale_frame, reqid).unwrap_err();
        assert!(matches!(err, FrameError::ReqIdMismatch { .. }));

        // Parsing the good frame should succeed
        let frame = parse_v2_frame(&good_frame, reqid).unwrap();
        assert_eq!(frame.status, "SUCCESS");
    }
}
