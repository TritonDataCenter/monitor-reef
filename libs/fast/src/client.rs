// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! This module provides the interface for creating Fast clients.

use std::io::{Error, ErrorKind, Read, Write};
use std::net::TcpStream;

use bytes::BytesMut;
use serde_json::Value;

use crate::protocol;
use crate::protocol::{
    FastMessage, FastMessageData, FastMessageId, FastMessageServerError,
    FastMessageStatus, FastParseError,
};

enum BufferAction {
    Keep,
    Trim(usize),
    Done,
}

/// Send a message to a Fast server using the provided TCP stream.
pub fn send(
    method: String,
    args: Value,
    msg_id: &mut FastMessageId,
    stream: &mut TcpStream,
) -> Result<usize, Error> {
    // FastMessageId iterator always returns Some(id) by design (infinite iterator).
    let id = msg_id.next().unwrap_or(0) as u32;
    let msg = FastMessage::data(id, FastMessageData::new(method, args));
    let mut write_buf = BytesMut::new();
    match protocol::encode_msg(&msg, &mut write_buf) {
        Ok(_) => {
            let bytes = stream.write(write_buf.as_ref())?;
            stream.flush()?;
            Ok(bytes)
        }
        Err(err_str) => Err(Error::other(err_str)),
    }
}

/// Receive a message from a Fast server on the provided TCP stream and call
/// `response_handler` on the response.
pub fn receive<F>(
    stream: &mut TcpStream,
    mut response_handler: F,
) -> Result<usize, Error>
where
    F: FnMut(&FastMessage) -> Result<(), Error>,
{
    let mut stream_end = false;
    let mut msg_buf: Vec<u8> = Vec::new();
    let mut total_bytes = 0;
    let mut result = Ok(total_bytes);

    while !stream_end {
        let mut read_buf = [0; 128];
        match stream.read(&mut read_buf) {
            Ok(0) => {
                result = Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "Received EOF (0 bytes) from server",
                ));
                stream_end = true;
            }
            Ok(byte_count) => {
                total_bytes += byte_count;
                msg_buf.extend_from_slice(&read_buf[0..byte_count]);
                match parse_and_handle_messages(
                    msg_buf.as_slice(),
                    &mut response_handler,
                ) {
                    Ok(BufferAction::Keep) => (),
                    Ok(BufferAction::Trim(rest_offset)) => {
                        let truncate_bytes = msg_buf.len() - rest_offset;
                        msg_buf.rotate_left(rest_offset);
                        msg_buf.truncate(truncate_bytes);
                        result = Ok(total_bytes);
                    }
                    Ok(BufferAction::Done) => stream_end = true,
                    Err(e) => {
                        result = Err(e);
                        stream_end = true
                    }
                }
            }
            Err(err) => {
                result = Err(err);
                stream_end = true
            }
        }
    }
    result
}

fn parse_and_handle_messages<F>(
    read_buf: &[u8],
    response_handler: &mut F,
) -> Result<BufferAction, Error>
where
    F: FnMut(&FastMessage) -> Result<(), Error>,
{
    let mut offset = 0;
    let mut done = false;

    let mut result = Ok(BufferAction::Keep);

    while !done {
        match FastMessage::parse(&read_buf[offset..]) {
            Ok(ref fm) if fm.status == FastMessageStatus::End => {
                result = Ok(BufferAction::Done);
                done = true;
            }
            Ok(fm) => {
                // msg_size is always Some for non-End status messages (see protocol.rs)
                let msg_size = fm.msg_size.ok_or_else(|| {
                    Error::other(
                        "Protocol error: msg_size was None for non-End message",
                    )
                })?;
                offset += msg_size;
                match fm.status {
                    FastMessageStatus::Data | FastMessageStatus::End => {
                        if let Err(e) = response_handler(&fm) {
                            result = Err(e);
                            done = true;
                        } else {
                            result = Ok(BufferAction::Trim(offset));
                        }
                    }
                    FastMessageStatus::Error => {
                        result =
                            serde_json::from_value::<FastMessageServerError>(
                                fm.data.d.clone(),
                            )
                            .map_err(|deser_err| {
                                Error::other(format!(
                                    "Failed to parse server error: {}. Raw: {}",
                                    deser_err, fm.data.d
                                ))
                            })
                            .and_then(|e| Err(e.into()));

                        done = true;
                    }
                }
            }
            Err(FastParseError::NotEnoughBytes(_bytes)) => {
                done = true;
            }
            Err(FastParseError::IOError(e)) => {
                result = Err(e);
                done = true;
            }
        }
    }

    result
}
