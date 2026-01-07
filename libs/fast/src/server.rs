// Copyright 2019 Joyent, Inc.

//! This module provides the interface for creating Fast servers.

use std::io::Error;

use futures::{SinkExt, StreamExt};
use serde_json::json;
use slog::{Drain, Logger, debug, error, o};
use tokio::net::TcpStream;
use tokio_util::codec::Decoder;

use crate::protocol::{FastMessage, FastMessageData, FastRpc};

/// Handle Fast protocol requests on a TCP stream.
///
/// This async function processes incoming Fast messages and sends responses.
pub async fn handle_connection<F>(
    socket: TcpStream,
    mut response_handler: F,
    log: Option<&Logger>,
) -> Result<(), Error>
where
    F: FnMut(&FastMessage, &Logger) -> Result<Vec<FastMessage>, Error> + Send,
{
    let (mut tx, mut rx) = FastRpc.framed(socket).split();

    // If no logger was provided use the slog StdLog drain by default
    let log = log
        .cloned()
        .unwrap_or_else(|| Logger::root(slog_stdlog::StdLog.fuse(), o!()));

    while let Some(result) = rx.next().await {
        match result {
            Ok(msgs) => {
                debug!(log, "processing fast message");
                let responses = respond(msgs, &mut response_handler, &log);
                if let Err(e) = tx.send(responses).await {
                    error!(log, "failed to send response"; "err" => %e);
                    return Err(e);
                }
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
) -> Vec<FastMessage>
where
    F: FnMut(&FastMessage, &Logger) -> Result<Vec<FastMessage>, Error> + Send,
{
    debug!(log, "responding to {} messages", msgs.len());

    let mut responses: Vec<FastMessage> = Vec::new();

    for msg in msgs {
        match response_handler(&msg, log) {
            Ok(mut response) => {
                // Make sure there is room in responses to fit another response plus an
                // end message
                let responses_len = responses.len();
                let response_len = response.len();
                let responses_capacity = responses.capacity();
                if responses_len + response_len > responses_capacity {
                    let needed_capacity =
                        responses_len + response_len - responses_capacity;
                    responses.reserve(needed_capacity);
                }

                // Add all response messages for this message to the vector of
                // all responses
                response.drain(..).for_each(|r| {
                    responses.push(r);
                });

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

    responses
}
