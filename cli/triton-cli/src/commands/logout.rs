// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton logout` — revoke the server-side session for this profile
//! and remove the cached token file.
//!
//! Best-effort server call: if the revoke request fails (network,
//! already-expired token, etc.) we still remove the local token file
//! so the user isn't stuck with a stale credential they can't use.
//! The server-side revoke invalidates refresh tokens for the
//! account, which prevents issuing new access tokens; the currently-
//! cached access token remains valid until its exp, which the TTL
//! bounds anyway.

use anyhow::Result;
use triton_gateway_client::TypedClient;

use crate::commands::login;
use crate::config::{Profile, paths};

pub async fn run(client: &TypedClient, profile: &Profile, use_json: bool) -> Result<()> {
    // No cached token → nothing to do on the server, and nothing to
    // delete locally (or a file that's not fresh-enough to have made
    // it through `build_client`).
    let had_token = login::load_if_fresh(&profile.name).await.is_some();
    if !had_token {
        if use_json {
            println!(
                "{}",
                serde_json::json!({
                    "profile": profile.name,
                    "state": "not_logged_in",
                })
            );
        } else {
            eprintln!("Not logged in to profile '{}'.", profile.name);
        }
        return Ok(());
    }

    // Best-effort revoke. Server-side failure shouldn't block us from
    // clearing the local file -- the user's intent is "end my session
    // here", and a stale local token is the worst UX.
    let revoke_result = client.inner().auth_logout().send().await;
    let revoked = revoke_result.is_ok();
    if let Err(e) = &revoke_result {
        tracing::debug!("server-side logout failed (local token will still be removed): {e}");
    }

    // Remove the token file. Missing-is-fine; other errors propagate
    // so the user knows the local state didn't fully clean up.
    let path = paths::token_path(&profile.name)?;
    match tokio::fs::remove_file(&path).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            anyhow::bail!(
                "server-side logout {}; failed to remove {}: {e}",
                if revoked { "succeeded" } else { "failed" },
                path.display()
            );
        }
    }

    if use_json {
        println!(
            "{}",
            serde_json::json!({
                "profile": profile.name,
                "server_revoked": revoked,
                "token_file_removed": true,
            })
        );
    } else {
        if revoked {
            println!("Logged out of {}.", profile.url);
        } else {
            println!(
                "Local token for {} removed; server-side revoke failed (session may have \
                 already been invalid).",
                profile.name
            );
        }
        println!("Removed {}", path.display());
    }

    Ok(())
}
