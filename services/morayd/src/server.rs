// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Fast-protocol TCP server.
//!
//! Each accepted connection gets its own task that runs a
//! `Framed<TcpStream, FastCodec>` read-loop, dispatches every request via
//! [`crate::rpc::dispatch`], and writes back the data + end (or error)
//! frames. Concurrency between connections is native tokio; concurrency
//! within a connection is serial (clients expect responses to be in flight
//! in order per msgid).

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

use crate::fast::{FastCodec, FastStatus};
use crate::rpc;
use crate::store::MorayStore;

/// Bind and serve on `listen_addr` (e.g., "0.0.0.0:2020") using the given
/// store. Returns only on fatal listener error.
pub async fn run<S, A>(store: Arc<S>, listen_addr: A) -> std::io::Result<()>
where
    S: MorayStore,
    A: tokio::net::ToSocketAddrs,
{
    let listener = TcpListener::bind(listen_addr).await?;
    let local = listener.local_addr()?;
    info!(%local, "morayd listening");

    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                error!(err = %e, "accept failed");
                continue;
            }
        };
        let store = store.clone();
        tokio::spawn(async move {
            debug!(%peer, "client connected");
            if let Err(e) = connection(store, sock).await {
                warn!(%peer, err = %e, "connection ended with error");
            } else {
                debug!(%peer, "client disconnected");
            }
        });
    }
}

async fn connection<S: MorayStore>(
    store: Arc<S>,
    sock: tokio::net::TcpStream,
) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    let mut framed = Framed::new(sock, FastCodec);

    while let Some(frame) = framed.next().await {
        let msg = match frame {
            Ok(m) => m,
            Err(e) => {
                warn!(err = ?e, "fast codec error");
                break;
            }
        };
        // Only Data-status frames carry requests. Clients don't send End/
        // Error frames at us; drop them.
        if msg.status != FastStatus::Data {
            continue;
        }
        let responses = rpc::dispatch(store.clone(), &msg).await;
        for r in responses {
            if let Err(e) = framed.send(r).await {
                warn!(err = ?e, "send failed");
                return Ok(());
            }
        }
    }
    Ok(())
}
