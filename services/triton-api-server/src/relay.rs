// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Server-side relay state: tracks the single registered agent tunnel and
//! drives the yamux client connection on that tunnel.

use anyhow::Result;
use std::future::poll_fn;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use yamux::{Connection, Stream};

pub struct RelayState {
    tunnel: Mutex<Option<TunnelHandle>>,
}

/// Handle to a live agent connection. The driver task owns the yamux
/// `Connection` and accepts stream-open requests through this channel.
pub struct TunnelHandle {
    pub open_stream: mpsc::Sender<oneshot::Sender<Result<Stream>>>,
}

impl RelayState {
    pub fn new() -> Self {
        Self {
            tunnel: Mutex::new(None),
        }
    }

    /// Register a new agent tunnel, replacing any previous one.
    pub fn register(&self, handle: TunnelHandle) {
        let mut guard = self.tunnel.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            warn!("replacing existing relay tunnel handle (agent reconnected)");
        }
        *guard = Some(handle);
    }

    /// Clear the registered tunnel (called when the agent disconnects).
    pub fn clear(&self) {
        let mut guard = self.tunnel.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        info!("relay tunnel handle cleared");
    }

    /// Ask the driver task to open a new yamux stream targeted at the agent.
    pub async fn open_stream(&self) -> Result<Stream> {
        let tx = {
            let guard = self.tunnel.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_ref() {
                Some(h) => h.open_stream.clone(),
                None => anyhow::bail!("no relay agent is currently registered"),
            }
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(reply_tx)
            .await
            .map_err(|_| anyhow::anyhow!("relay driver task has stopped"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("relay driver task has stopped"))?
    }
}

/// Drive the agent-side yamux connection.
///
/// Runs the yamux client connection: opens outbound streams on demand via the
/// `req_rx` channel, and drives inbound to process protocol frames (the agent
/// should not open streams, but we still need to drain to handle keepalives
/// and window updates).
///
/// Clears the relay handle when the connection closes.
pub async fn run_agent_connection<T>(
    mut conn: Connection<T>,
    mut req_rx: mpsc::Receiver<oneshot::Sender<Result<Stream>>>,
    relay: Arc<RelayState>,
) where
    T: futures_util::io::AsyncRead + futures_util::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut pending: Option<oneshot::Sender<Result<Stream>>> = None;

    poll_fn(move |cx| {
        use std::task::Poll;

        // Accept a stream-open request if none is pending.
        if pending.is_none() {
            match req_rx.poll_recv(cx) {
                Poll::Ready(Some(tx)) => pending = Some(tx),
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => {}
            }
        }

        // Try to open the outbound stream toward the agent.
        if pending.is_some() {
            match conn.poll_new_outbound(cx) {
                Poll::Ready(Ok(stream)) => {
                    if let Some(tx) = pending.take() {
                        let _ = tx.send(Ok(stream));
                    }
                }
                Poll::Ready(Err(e)) => {
                    if let Some(tx) = pending.take() {
                        let _ = tx.send(Err(anyhow::anyhow!("{e}")));
                    }
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }
        }

        // Drive inbound to process protocol frames and keepalives.
        loop {
            match conn.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(s))) => drop(s), // agent should not open streams
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        Poll::Pending
    })
    .await;

    relay.clear();
}
