// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! This module provides the interface for creating Fast servers.

use std::io::Error;

use futures::{SinkExt, StreamExt};
use serde_json::json;
use slog::{Drain, Logger, debug, error, o};
use tokio::net::TcpStream;
use tokio_util::codec::Decoder;

use crate::protocol::{FastMessage, FastMessageData, FastRpc};

/// Handle a Fast protocol connection over a TcpStream.
///
/// This function processes incoming Fast RPC requests, invokes the provided
/// response handler for each message, and sends responses back to the client.
pub async fn handle_connection<F>(
    socket: TcpStream,
    mut response_handler: F,
    log: Option<&Logger>,
) -> Result<(), Error>
where
    F: FnMut(&FastMessage, &Logger) -> Result<Vec<FastMessage>, Error> + Send,
{
    let (mut tx, mut rx) = FastRpc.framed(socket).split();

    let log = log
        .cloned()
        .unwrap_or_else(|| Logger::root(slog_stdlog::StdLog.fuse(), o!()));

    while let Some(result) = rx.next().await {
        match result {
            Ok(msgs) => {
                debug!(log, "processing fast message");
                let responses = respond(msgs, &mut response_handler, &log)?;
                tx.send(responses).await?;
                debug!(log, "transmitted response to client");
            }
            Err(e) => {
                error!(log, "failed to process connection"; "err" => %e);
                return Err(e);
            }
        }
    }

    Ok(())
}

fn respond<F>(
    msgs: Vec<FastMessage>,
    response_handler: &mut F,
    log: &Logger,
) -> Result<Vec<FastMessage>, Error>
where
    F: FnMut(&FastMessage, &Logger) -> Result<Vec<FastMessage>, Error> + Send,
{
    debug!(log, "responding to {} messages", msgs.len());

    let mut responses: Vec<FastMessage> = Vec::new();

    for msg in msgs {
        match response_handler(&msg, log) {
            Ok(mut response) => {
                responses.append(&mut response);
                debug!(log, "generated response");
                let method = msg.data.m.name.clone();
                responses.push(FastMessage::end(msg.id, method));
            }
            Err(err) => {
                let method = msg.data.m.name.clone();
                let value = json!({
                    "name": "FastError",
                    "message": err.to_string()
                });

                let err_msg = FastMessage::error(
                    msg.id,
                    FastMessageData::new(method, value),
                );
                responses.push(err_msg);
            }
        }
    }

    Ok(responses)
}
