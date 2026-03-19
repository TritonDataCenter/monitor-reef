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

use std::fs::File;
use std::io::Read;
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

use crate::Response;
use crate::transport::{Transport, TransportError};

/// Timeout for V1 commands and protocol negotiation (6 seconds).
const RECV_TIMEOUT_MS: u64 = 6_000;

/// Timeout for V2 operations (45 seconds, allows for slower PUT).
const RECV_TIMEOUT_MS_V2: u64 = 45_000;

/// Maximum number of timeout-and-reset retries before giving up.
const MAX_RETRIES: u32 = 3;

/// Maximum number of stale V2 frames to discard before giving up.
const MAX_STALE_FRAMES: u32 = 5;

/// Protocol handler for metadata operations.
pub struct Protocol {
    transport: Transport,
    version: u8,
}

impl Protocol {
    /// Initialize: open transport, negotiate protocol version.
    pub fn init() -> Result<Self> {
        let mut transport = Transport::open()?;
        let version = negotiate(&mut transport)?;
        Ok(Self { transport, version })
    }

    /// The negotiated protocol version (1 or 2).
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Execute a DELETE command.
    ///
    /// Requires V2 protocol support.
    pub fn delete(&mut self, key: &str) -> Result<Response> {
        if self.version < 2 {
            bail!(
                "metadata service does not support V2 protocol \
                 (required for DELETE)"
            );
        }
        self.execute("DELETE", Some(key))
    }

    /// Execute a PUT command, encoding the key and value per protocol.
    ///
    /// The V2 PUT wire format requires `base64(key) + " " + base64(value)`
    /// as the command argument. This method handles that encoding so
    /// callers can pass raw strings.
    pub fn put(&mut self, key: &str, value: &str) -> Result<Response> {
        if self.version < 2 {
            bail!(
                "metadata service does not support V2 protocol \
                 (required for PUT)"
            );
        }
        let arg = format!(
            "{} {}",
            STANDARD.encode(key),
            STANDARD.encode(value),
        );
        self.execute("PUT", Some(&arg))
    }

    /// Execute a metadata command with automatic retry on timeout.
    ///
    /// On timeout, the protocol is reset (transport reconnected and
    /// V2 re-negotiated) and the command is retried.
    pub fn execute(
        &mut self,
        command: &str,
        arg: Option<&str>,
    ) -> Result<Response> {
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
                        eprintln!(
                            "receive timeout, resetting \
                             protocol (attempt {retries}/{MAX_RETRIES})..."
                        );
                        self.reset()?;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    fn try_execute(
        &mut self,
        command: &str,
        arg: Option<&str>,
    ) -> Result<Response> {
        if self.version >= 2 {
            self.execute_v2(command, arg)
        } else {
            self.execute_v1(command, arg)
        }
    }

    /// Execute a V1 protocol command.
    fn execute_v1(
        &mut self,
        command: &str,
        arg: Option<&str>,
    ) -> Result<Response> {
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
                    let line =
                        self.transport.recv_line(RECV_TIMEOUT_MS)?;
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
    fn execute_v2(
        &mut self,
        command: &str,
        arg: Option<&str>,
    ) -> Result<Response> {
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
            let line =
                self.transport.recv_line(RECV_TIMEOUT_MS_V2)?;
            match parse_v2_frame(&line, &reqid) {
                Ok(frame) => {
                    return match frame.status.as_str() {
                        "SUCCESS" => {
                            let data = frame
                                .payload
                                .map(|p| decode_b64_payload(&p))
                                .transpose()?;
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
        self.version = negotiate(&mut self.transport)?;
        Ok(())
    }
}

/// Negotiate protocol version with the metadata service.
///
/// For serial transports, sends a reset sequence first (`\n` →
/// `invalid command`) to clear any stale state on the port.
fn negotiate(transport: &mut Transport) -> Result<u8> {
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
        Ok(ref line) if line == "V2_OK" => Ok(2),
        Ok(ref line) if line == "invalid command" => Ok(1),
        Ok(other) => {
            bail!("unexpected negotiation response: {other}")
        }
        Err(TransportError::Timeout) => {
            bail!("timeout during protocol negotiation")
        }
        Err(e) => Err(e.into()),
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
fn parse_v2_frame(
    line: &str,
    expected_reqid: &str,
) -> std::result::Result<V2Frame, FrameError> {
    let parse_body =
        || -> Result<(String, String, Option<String>)> {
            let mut parts = line.splitn(4, ' ');

            let marker = parts.next();
            let len_str = parts.next();
            let crc_str = parts.next();
            let body = parts.next();

            let (
                Some("V2"),
                Some(len_str),
                Some(crc_str),
                Some(body),
            ) = (marker, len_str, crc_str, body)
            else {
                bail!(
                    "invalid V2 frame: \
                     expected 'V2 <len> <crc> <body>'"
                );
            };

            let expected_len: usize =
                len_str.parse().map_err(|_| {
                    anyhow::anyhow!(
                        "invalid V2 frame length: {len_str}"
                    )
                })?;

            let expected_crc =
                u32::from_str_radix(crc_str, 16).map_err(|_| {
                    anyhow::anyhow!(
                        "invalid V2 frame CRC: {crc_str}"
                    )
                })?;

            if body.len() != expected_len {
                bail!(
                    "V2 frame length mismatch: \
                     header says {expected_len}, body is {}",
                    body.len()
                );
            }

            let actual_crc = crc32fast::hash(body.as_bytes());
            if actual_crc != expected_crc {
                bail!(
                    "V2 frame CRC mismatch: \
                     expected {expected_crc:08x}, \
                     got {actual_crc:08x}"
                );
            }

            let mut body_parts = body.splitn(3, ' ');
            let reqid = body_parts.next().ok_or_else(|| {
                anyhow::anyhow!("missing request ID in V2 frame")
            })?;
            let status = body_parts.next().ok_or_else(|| {
                anyhow::anyhow!("missing status in V2 frame")
            })?;
            let payload = body_parts.next().map(String::from);

            Ok((
                reqid.to_string(),
                status.to_string(),
                payload,
            ))
        };

    match parse_body() {
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

/// Decode a BASE64-encoded payload string.
fn decode_b64_payload(encoded: &str) -> Result<String> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|e| anyhow::anyhow!("invalid base64 in response: {e}"))?;
    String::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("response payload is not valid UTF-8: {e}"))
}

/// Generate an 8-character hex request ID for V2 protocol frames.
fn generate_request_id() -> Result<String> {
    let mut buf = [0u8; 4];

    // Try /dev/urandom first (available on all Unix platforms)
    if let Ok(mut f) = File::open("/dev/urandom")
        && f.read_exact(&mut buf).is_ok()
    {
        return Ok(format!("{:08x}", u32::from_ne_bytes(buf)));
    }

    // Fallback: derive from current time (should rarely happen)
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(0xdeadbeef);
    Ok(format!("{nanos:08x}"))
}

/// Check if an error is a transport timeout.
fn is_timeout(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TransportError>()
        .is_some_and(|te| matches!(te, TransportError::Timeout))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
        let frame =
            format!("V2 {} {crc:08x} {body}", body.len());

        let f = parse_v2_frame(&frame, reqid).unwrap();
        assert_eq!(f.status, "SUCCESS");
        let decoded =
            decode_b64_payload(&f.payload.unwrap()).unwrap();
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn test_parse_v2_frame_notfound() {
        let reqid = "abcd1234";
        let body = format!("{reqid} NOTFOUND");
        let crc = crc32fast::hash(body.as_bytes());
        let frame =
            format!("V2 {} {crc:08x} {body}", body.len());

        let f = parse_v2_frame(&frame, reqid).unwrap();
        assert_eq!(f.status, "NOTFOUND");
        assert!(f.payload.is_none());
    }

    #[test]
    fn test_parse_v2_frame_bad_crc() {
        let reqid = "dc4fae17";
        let body = format!("{reqid} SUCCESS");
        let frame =
            format!("V2 {} 00000000 {body}", body.len());

        let err = parse_v2_frame(&frame, reqid).unwrap_err();
        assert!(matches!(err, FrameError::Other(_)));
        assert!(format!("{err}").contains("CRC mismatch"));
    }

    #[test]
    fn test_parse_v2_frame_wrong_reqid() {
        let reqid = "dc4fae17";
        let body = format!("{reqid} SUCCESS");
        let crc = crc32fast::hash(body.as_bytes());
        let frame =
            format!("V2 {} {crc:08x} {body}", body.len());

        let err =
            parse_v2_frame(&frame, "00000000").unwrap_err();
        assert!(matches!(
            err,
            FrameError::ReqIdMismatch { .. }
        ));
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
}
