// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `triton whoami` — ask the tritonapi server to describe the current
//! Bearer session. Exercises `GET /v1/auth/session`.
//!
//! Requires a cached token: the endpoint only accepts Bearer, so if
//! there's no token for this profile (or the cached JWT looks
//! expired) we surface "not logged in" rather than trying to present
//! an SSH signature the server won't accept.

use anyhow::{Result, anyhow};
use triton_gateway_client::TypedClient;

use crate::commands::login;
use crate::config::Profile;
use crate::output::json;

pub async fn run(client: &TypedClient, profile: &Profile, use_json: bool) -> Result<()> {
    if login::load_if_fresh(&profile.name).await.is_none() {
        anyhow::bail!(
            "not logged in to profile '{}' (no fresh cached token). \
             Run 'triton -p {} login' first.",
            profile.name,
            profile.name
        );
    }

    let response = client
        .inner()
        .auth_session()
        .send()
        .await
        .map_err(|e| anyhow!("session lookup failed: {e}"))?;
    let session = response.into_inner();

    if use_json {
        json::print_json(&session)?;
    } else {
        println!("Profile:  {}", profile.name);
        println!("URL:      {}", profile.url);
        println!("Username: {}", session.user.username);
        println!("User ID:  {}", session.user.id);
        if session.user.is_admin {
            println!("Role:     operator / admin");
        }
    }
    Ok(())
}
