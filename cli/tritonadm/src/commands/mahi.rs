// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm mahi <subcommand>` — thin wrapper over the Mahi auth-cache
//! client (and its companion sitter client). Covers the full public Mahi HTTP
//! API (26 endpoints across lookup, AWS SigV4, STS, IAM) plus the two sitter
//! endpoints (ping, snapshot) via a nested `sitter` subcommand group.
//!
//! The Mahi sitter runs on port 8080 of the same mahi zone (the main service
//! is on port 80). On a Triton headnode this URL is derived automatically
//! from the SDC config; elsewhere supply `--mahi-sitter-url` /
//! `MAHI_SITTER_URL` explicitly.

use anyhow::{Context, Result};
use clap::Subcommand;
use mahi_client::Client;
use mahi_client::types;
use uuid::Uuid;

/// Convert a serde-serializable enum value to its wire-format string.
fn enum_to_display<T: serde::Serialize + std::fmt::Debug>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", val))
}

/// Parse a JSON string into a serde_json::Value.
fn parse_json(s: &str) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(s)?)
}

#[derive(Subcommand)]
pub enum MahiCommand {
    // ========================================================================
    // Lookup (classic)
    // ========================================================================
    /// Health check (GET /ping)
    Ping,

    /// Look up an account by login (GET /accounts?login=...)
    #[command(name = "get-account")]
    GetAccount {
        /// Account login
        #[arg(long)]
        login: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Look up an account by UUID (GET /accounts/{accountid})
    #[command(name = "get-account-by-uuid")]
    GetAccountByUuid {
        /// Account UUID
        accountid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Look up a sub-user by account login + user login (GET /users)
    #[command(name = "get-user")]
    GetUser {
        /// Parent account login
        #[arg(long)]
        account: String,
        /// Sub-user login
        #[arg(long)]
        login: String,
        /// When true, missing users return the account-only payload
        #[arg(long)]
        fallback: Option<bool>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Look up a sub-user by UUID (GET /users/{userid})
    #[command(name = "get-user-by-uuid")]
    GetUserByUuid {
        /// User UUID
        userid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// List the members of a role (GET /roles)
    #[command(name = "get-role-members")]
    GetRoleMembers {
        /// Account login
        #[arg(long)]
        account: String,
        /// Role name
        #[arg(long)]
        role: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Resolve names to UUIDs within an account (GET /uuids)
    #[command(name = "name-to-uuid")]
    NameToUuid {
        /// Account login
        #[arg(long)]
        account: String,
        /// Object type: role, user, or policy
        #[arg(long = "type", value_enum)]
        object_type: types::ObjectType,
        /// Name to resolve (repeat --name for multiple)
        #[arg(long)]
        name: Vec<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Resolve UUIDs to names (GET /names)
    #[command(name = "uuid-to-name")]
    UuidToName {
        /// UUID to resolve (repeat --uuid for multiple)
        #[arg(long)]
        uuid: Vec<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// List every approved account (GET /lookup)
    Lookup {
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Deprecated lookup aliases
    // ========================================================================
    /// Deprecated: look up an account by login via path (GET /account/{login})
    #[command(name = "get-account-old")]
    GetAccountOld {
        /// Account login
        account: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Deprecated: look up a sub-user by path (GET /user/{account}/{user})
    #[command(name = "get-user-old")]
    GetUserOld {
        /// Parent account login
        account: String,
        /// Sub-user login
        user: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Deprecated: POST /getUuid (same shape as name-to-uuid)
    #[command(name = "name-to-uuid-old")]
    NameToUuidOld {
        /// Account login
        #[arg(long)]
        account: String,
        /// Object type: role, user, or policy
        #[arg(long = "type", value_enum)]
        object_type: types::ObjectType,
        /// Name to resolve (repeat --name for multiple)
        #[arg(long)]
        name: Vec<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Deprecated: POST /getName (same shape as uuid-to-name)
    #[command(name = "uuid-to-name-old")]
    UuidToNameOld {
        /// UUID to resolve (repeat --uuid for multiple)
        #[arg(long)]
        uuid: Vec<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // AWS SigV4
    // ========================================================================
    /// Look up a principal by AWS access key (GET /aws-auth/{accesskeyid})
    #[command(name = "get-user-by-access-key")]
    GetUserByAccessKey {
        /// Access key id
        accesskeyid: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// Verify a SigV4 signature (POST /aws-verify)
    #[command(name = "verify-sig-v4")]
    VerifySigV4 {
        /// HTTP method of the original request
        #[arg(long)]
        method: String,
        /// URL of the original request
        #[arg(long)]
        url: String,
        /// JSON body to forward (defaults to {})
        #[arg(long)]
        body: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // STS (manta-only)
    // ========================================================================
    /// POST /sts/assume-role
    #[command(name = "sts-assume-role")]
    StsAssumeRole {
        /// Full JSON request body ({caller, RoleArn, RoleSessionName, ...})
        #[arg(long)]
        body: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// POST /sts/get-session-token
    #[command(name = "sts-get-session-token")]
    StsGetSessionToken {
        /// Full JSON request body ({caller, DurationSeconds?})
        #[arg(long)]
        body: String,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// POST /sts/get-caller-identity (returns XML)
    #[command(name = "sts-get-caller-identity")]
    StsGetCallerIdentity {
        /// Full JSON request body ({caller})
        #[arg(long)]
        body: String,
    },

    // ========================================================================
    // IAM (manta-only)
    // ========================================================================
    /// POST /iam/create-role
    #[command(name = "iam-create-role")]
    IamCreateRole {
        /// Role name
        #[arg(long)]
        role_name: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Assume-role policy document (JSON string)
        #[arg(long)]
        assume_role_policy_document: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// IAM path (default "/")
        #[arg(long)]
        path: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// GET /iam/get-role/{roleName}
    #[command(name = "iam-get-role")]
    IamGetRole {
        /// Role name
        role_name: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// POST /iam/put-role-policy
    #[command(name = "iam-put-role-policy")]
    IamPutRolePolicy {
        /// Role name
        #[arg(long)]
        role_name: String,
        /// Policy name
        #[arg(long)]
        policy_name: String,
        /// Policy document (raw IAM JSON string)
        #[arg(long)]
        policy_document: String,
        /// Manta policy object (JSON: {id, name, rules})
        #[arg(long)]
        manta_policy: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// DELETE /iam/delete-role/{roleName}
    #[command(name = "iam-delete-role")]
    IamDeleteRole {
        /// Role name
        role_name: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// DELETE /iam/delete-role-policy
    #[command(name = "iam-delete-role-policy")]
    IamDeleteRolePolicy {
        /// Role name
        #[arg(long)]
        role_name: String,
        /// Policy name
        #[arg(long)]
        policy_name: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// GET /iam/list-roles
    #[command(name = "iam-list-roles")]
    IamListRoles {
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Max items to return (default 100)
        #[arg(long)]
        max_items: Option<u32>,
        /// Pagination marker
        #[arg(long)]
        marker: Option<String>,
        /// Alternative pagination marker (`startingToken`)
        #[arg(long)]
        starting_token: Option<String>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// GET /iam/list-role-policies/{roleName}
    #[command(name = "iam-list-role-policies")]
    IamListRolePolicies {
        /// Role name
        role_name: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Pagination marker
        #[arg(long)]
        marker: Option<String>,
        /// Max items (upstream uses lowercase `maxitems`)
        #[arg(long)]
        maxitems: Option<u32>,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    /// GET /iam/get-role-policy/{roleName}/{policyName}
    #[command(name = "iam-get-role-policy")]
    IamGetRolePolicy {
        /// Role name
        role_name: String,
        /// Policy name
        policy_name: String,
        /// Account UUID
        #[arg(long)]
        account_uuid: Uuid,
        /// Output raw JSON
        #[arg(long)]
        raw: bool,
    },

    // ========================================================================
    // Sitter subcommand group
    // ========================================================================
    /// Mahi sitter (replicator admin) commands
    Sitter {
        #[command(subcommand)]
        command: MahiSitterCommand,
    },
}

#[derive(Subcommand)]
pub enum MahiSitterCommand {
    /// Sitter health check (GET /ping on the sitter port)
    Ping,

    /// Stream the Redis `dump.rdb` snapshot (GET /snapshot)
    Snapshot {
        /// Destination path (stdout if unset)
        #[arg(long, short)]
        output: Option<String>,
    },
}

impl MahiCommand {
    pub async fn run(
        self,
        mahi_url: Result<String>,
        mahi_sitter_url: Result<String>,
    ) -> Result<()> {
        // Sitter subcommands need only the sitter URL, not the main one.
        if let MahiCommand::Sitter { command } = self {
            let sitter_url = mahi_sitter_url?;
            return command.run(&sitter_url).await;
        }

        let mahi_url = mahi_url?;
        let http = triton_tls::build_http_client(false)
            .await
            .context("failed to build HTTP client")?;
        let client = Client::new_with_client(&mahi_url, http);

        match self {
            // ================================================================
            // Lookup
            // ================================================================
            MahiCommand::Ping => {
                client
                    .ping()
                    .send()
                    .await
                    .context("failed to call GET /ping")?;
                println!("ok");
            }
            MahiCommand::GetAccount { login, raw } => {
                let mut req = client.get_account();
                if let Some(l) = login {
                    req = req.login(l);
                }
                let resp = req.send().await.context("failed to call GET /accounts")?;
                let auth = resp.into_inner();
                print_auth_info(&auth, raw)?;
            }
            MahiCommand::GetAccountByUuid { accountid, raw } => {
                let resp = client
                    .get_account_by_uuid()
                    .accountid(accountid)
                    .send()
                    .await
                    .context("failed to call GET /accounts/{accountid}")?;
                let auth = resp.into_inner();
                print_auth_info(&auth, raw)?;
            }
            MahiCommand::GetUser {
                account,
                login,
                fallback,
                raw,
            } => {
                let mut req = client.get_user().account(account).login(login);
                if let Some(f) = fallback {
                    req = req.fallback(f);
                }
                let resp = req.send().await.context("failed to call GET /users")?;
                let auth = resp.into_inner();
                print_auth_info(&auth, raw)?;
            }
            MahiCommand::GetUserByUuid { userid, raw } => {
                let resp = client
                    .get_user_by_uuid()
                    .userid(userid)
                    .send()
                    .await
                    .context("failed to call GET /users/{userid}")?;
                let auth = resp.into_inner();
                print_auth_info(&auth, raw)?;
            }
            MahiCommand::GetRoleMembers { account, role, raw } => {
                let mut req = client.get_role_members().account(account);
                if let Some(r) = role {
                    req = req.role(r);
                }
                let resp = req.send().await.context("failed to call GET /roles")?;
                let auth = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&auth)?);
                } else if let Some(role) = &auth.role {
                    println!("role: {} ({})", role.name, role.uuid);
                    if let Some(members) = &role.members {
                        println!("members: {}", members.len());
                    }
                } else {
                    println!("account: {} ({})", auth.account.login, auth.account.uuid);
                    println!("(no role populated)");
                }
            }
            MahiCommand::NameToUuid {
                account,
                object_type,
                name,
                raw,
            } => {
                let mut req = client.name_to_uuid().account(account).type_(object_type);
                if !name.is_empty() {
                    req = req.name(name);
                }
                let resp = req.send().await.context("failed to call GET /uuids")?;
                let body = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&body)?);
                } else {
                    println!("account: {}", body.account);
                    if let Some(uuids) = &body.uuids {
                        for (name, uuid) in uuids {
                            println!("  {name} -> {uuid}");
                        }
                    }
                }
            }
            MahiCommand::UuidToName { uuid, raw } => {
                let mut req = client.uuid_to_name();
                if !uuid.is_empty() {
                    req = req.uuid(uuid);
                }
                let resp = req.send().await.context("failed to call GET /names")?;
                let body = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&body)?);
                } else {
                    for (uuid, name) in &body {
                        println!("{uuid} -> {name}");
                    }
                }
            }
            MahiCommand::Lookup { raw } => {
                let resp = client
                    .lookup()
                    .send()
                    .await
                    .context("failed to call GET /lookup")?;
                let body = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&body)?);
                } else {
                    for (uuid, entry) in &body {
                        println!("{uuid} {} approved={}", entry.login, entry.approved);
                    }
                }
            }

            // ================================================================
            // Deprecated lookup
            // ================================================================
            MahiCommand::GetAccountOld { account, raw } => {
                let resp = client
                    .get_account_old()
                    .account(account)
                    .send()
                    .await
                    .context("failed to call GET /account/{account}")?;
                let auth = resp.into_inner();
                print_auth_info(&auth, raw)?;
            }
            MahiCommand::GetUserOld { account, user, raw } => {
                let resp = client
                    .get_user_old()
                    .account(account)
                    .user(user)
                    .send()
                    .await
                    .context("failed to call GET /user/{account}/{user}")?;
                let auth = resp.into_inner();
                print_auth_info(&auth, raw)?;
            }
            MahiCommand::NameToUuidOld {
                account,
                object_type,
                name,
                raw,
            } => {
                let body_name = match name.len() {
                    0 => None,
                    1 => name.into_iter().next().map(types::StringOrVec::String),
                    _ => Some(types::StringOrVec::Array(name)),
                };
                let body = types::NameToUuidBody {
                    account,
                    type_: object_type,
                    name: body_name,
                };
                let resp = client
                    .name_to_uuid_old()
                    .body(body)
                    .send()
                    .await
                    .context("failed to call POST /getUuid")?;
                let body = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&body)?);
                } else {
                    println!("account: {}", body.account);
                    if let Some(uuids) = &body.uuids {
                        for (name, uuid) in uuids {
                            println!("  {name} -> {uuid}");
                        }
                    }
                }
            }
            MahiCommand::UuidToNameOld { uuid, raw } => {
                if uuid.is_empty() {
                    anyhow::bail!("at least one --uuid value is required");
                }
                let uuid_body = if uuid.len() == 1 {
                    // Pre-checked non-empty above; pick the single element.
                    match uuid.into_iter().next() {
                        Some(v) => types::StringOrVec::String(v),
                        None => unreachable!("len==1 implies a first element"),
                    }
                } else {
                    types::StringOrVec::Array(uuid)
                };
                let body = types::UuidToNameBody { uuid: uuid_body };
                let resp = client
                    .uuid_to_name_old()
                    .body(body)
                    .send()
                    .await
                    .context("failed to call POST /getName")?;
                let body = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&body)?);
                } else {
                    for (uuid, name) in &body {
                        println!("{uuid} -> {name}");
                    }
                }
            }

            // ================================================================
            // AWS SigV4
            // ================================================================
            MahiCommand::GetUserByAccessKey { accesskeyid, raw } => {
                let resp = client
                    .get_user_by_access_key()
                    .accesskeyid(accesskeyid)
                    .send()
                    .await
                    .context("failed to call GET /aws-auth/{accesskeyid}")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "account: {} ({})",
                        result.account.login, result.account.uuid
                    );
                    if let Some(user) = &result.user {
                        println!("user: {} ({})", user.login, user.uuid);
                    }
                    if let Some(is_temp) = result.is_temporary_credential {
                        println!("temporary: {is_temp}");
                    }
                    if let Some(role) = &result.assumed_role {
                        println!("assumedRole: {}", role.arn);
                    }
                }
            }
            MahiCommand::VerifySigV4 {
                method,
                url,
                body,
                raw,
            } => {
                let body_value = match body {
                    Some(s) => parse_json(&s).context("failed to parse --body as JSON")?,
                    None => serde_json::json!({}),
                };
                let resp = client
                    .verify_sig_v4()
                    .method(method)
                    .url(url)
                    .body(body_value)
                    .send()
                    .await
                    .context("failed to call POST /aws-verify")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("valid: {}", result.valid);
                    println!("accessKeyId: {}", result.access_key_id);
                    println!("userUuid: {}", result.user_uuid);
                    if let Some(is_temp) = result.is_temporary_credential {
                        println!("temporary: {is_temp}");
                    }
                }
            }

            // ================================================================
            // STS
            // ================================================================
            MahiCommand::StsAssumeRole { body, raw } => {
                let req: types::AssumeRoleRequest =
                    serde_json::from_str(&body).context("failed to parse --body")?;
                let resp = client
                    .sts_assume_role()
                    .body(req)
                    .send()
                    .await
                    .context("failed to call POST /sts/assume-role")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    let creds = &result.assume_role_response.assume_role_result.credentials;
                    let user = &result
                        .assume_role_response
                        .assume_role_result
                        .assumed_role_user;
                    println!("assumedRole: {}", user.arn);
                    println!("accessKeyId: {}", creds.access_key_id);
                    println!("expiration: {}", creds.expiration);
                }
            }
            MahiCommand::StsGetSessionToken { body, raw } => {
                let req: types::GetSessionTokenRequest =
                    serde_json::from_str(&body).context("failed to parse --body")?;
                let resp = client
                    .sts_get_session_token()
                    .body(req)
                    .send()
                    .await
                    .context("failed to call POST /sts/get-session-token")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    let creds = &result
                        .get_session_token_response
                        .get_session_token_result
                        .credentials;
                    println!("accessKeyId: {}", creds.access_key_id);
                    println!("expiration: {}", creds.expiration);
                }
            }
            MahiCommand::StsGetCallerIdentity { body } => {
                let req: types::GetCallerIdentityRequest =
                    serde_json::from_str(&body).context("failed to parse --body")?;
                let resp = client
                    .sts_get_caller_identity()
                    .body(req)
                    .send()
                    .await
                    .context("failed to call POST /sts/get-caller-identity")?;
                let stream = resp.into_inner();
                use futures_util::TryStreamExt;
                let chunks: Vec<bytes::Bytes> = stream.into_inner().try_collect().await?;
                let mut data = Vec::new();
                for chunk in chunks {
                    data.extend_from_slice(&chunk);
                }
                let xml = String::from_utf8(data)
                    .context("sts_get_caller_identity returned non-UTF-8 bytes")?;
                println!("{xml}");
            }

            // ================================================================
            // IAM
            // ================================================================
            MahiCommand::IamCreateRole {
                role_name,
                account_uuid,
                assume_role_policy_document,
                description,
                path,
                raw,
            } => {
                let body = types::CreateRoleRequest {
                    role_name,
                    account_uuid,
                    assume_role_policy_document,
                    description,
                    path,
                };
                let resp = client
                    .iam_create_role()
                    .body(body)
                    .send()
                    .await
                    .context("failed to call POST /iam/create-role")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    print_iam_role(&result.role);
                }
            }
            MahiCommand::IamGetRole {
                role_name,
                account_uuid,
                raw,
            } => {
                let resp = client
                    .iam_get_role()
                    .role_name(role_name)
                    .account_uuid(account_uuid)
                    .send()
                    .await
                    .context("failed to call GET /iam/get-role/{roleName}")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    print_iam_role(&result.role);
                }
            }
            MahiCommand::IamPutRolePolicy {
                role_name,
                policy_name,
                policy_document,
                manta_policy,
                account_uuid,
                raw,
            } => {
                let manta: types::MantaPolicy = serde_json::from_str(&manta_policy)
                    .context("failed to parse --manta-policy as JSON")?;
                let body = types::PutRolePolicyRequest {
                    role_name,
                    policy_name,
                    policy_document,
                    manta_policy: manta,
                    account_uuid,
                };
                let resp = client
                    .iam_put_role_policy()
                    .body(body)
                    .send()
                    .await
                    .context("failed to call POST /iam/put-role-policy")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "{}: role={} policy={}",
                        result.message, result.role_name, result.policy_name
                    );
                }
            }
            MahiCommand::IamDeleteRole {
                role_name,
                account_uuid,
                raw,
            } => {
                let resp = client
                    .iam_delete_role()
                    .role_name(role_name)
                    .account_uuid(account_uuid)
                    .send()
                    .await
                    .context("failed to call DELETE /iam/delete-role/{roleName}")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{}: role={}", result.message, result.role_name);
                }
            }
            MahiCommand::IamDeleteRolePolicy {
                role_name,
                policy_name,
                account_uuid,
                raw,
            } => {
                let resp = client
                    .iam_delete_role_policy()
                    .role_name(role_name)
                    .policy_name(policy_name)
                    .account_uuid(account_uuid)
                    .send()
                    .await
                    .context("failed to call DELETE /iam/delete-role-policy")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "{}: role={} policy={}",
                        result.message, result.role_name, result.policy_name
                    );
                }
            }
            MahiCommand::IamListRoles {
                account_uuid,
                max_items,
                marker,
                starting_token,
                raw,
            } => {
                let mut req = client.iam_list_roles().account_uuid(account_uuid);
                if let Some(m) = max_items {
                    req = req.max_items(m);
                }
                if let Some(m) = marker {
                    req = req.marker(m);
                }
                if let Some(t) = starting_token {
                    req = req.starting_token(t);
                }
                let resp = req
                    .send()
                    .await
                    .context("failed to call GET /iam/list-roles")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    for role in &result.roles {
                        println!("{} {}", role.role_id, role.role_name);
                    }
                    println!(
                        "IsTruncated={} Marker={}",
                        result.is_truncated,
                        result.marker.as_deref().unwrap_or("-")
                    );
                }
            }
            MahiCommand::IamListRolePolicies {
                role_name,
                account_uuid,
                marker,
                maxitems,
                raw,
            } => {
                let mut req = client
                    .iam_list_role_policies()
                    .role_name(role_name)
                    .account_uuid(account_uuid);
                if let Some(m) = marker {
                    req = req.marker(m);
                }
                if let Some(m) = maxitems {
                    req = req.maxitems(m);
                }
                let resp = req
                    .send()
                    .await
                    .context("failed to call GET /iam/list-role-policies/{roleName}")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    for name in &result.policy_names {
                        println!("{name}");
                    }
                    println!(
                        "IsTruncated={} Marker={}",
                        result.is_truncated,
                        result.marker.as_deref().unwrap_or("-")
                    );
                }
            }
            MahiCommand::IamGetRolePolicy {
                role_name,
                policy_name,
                account_uuid,
                raw,
            } => {
                let resp = client
                    .iam_get_role_policy()
                    .role_name(role_name)
                    .policy_name(policy_name)
                    .account_uuid(account_uuid)
                    .send()
                    .await
                    .context("failed to call GET /iam/get-role-policy/{roleName}/{policyName}")?;
                let result = resp.into_inner();
                if raw {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("role: {}", result.role_name);
                    println!("policy: {}", result.policy_name);
                    println!("document: {}", result.policy_document);
                }
            }

            // Already handled at the top of run().
            MahiCommand::Sitter { .. } => unreachable!(),
        }

        Ok(())
    }
}

impl MahiSitterCommand {
    pub async fn run(self, sitter_url: &str) -> Result<()> {
        let http = triton_tls::build_http_client(false)
            .await
            .context("failed to build HTTP client")?;
        let client = mahi_sitter_client::Client::new_with_client(sitter_url, http);

        match self {
            MahiSitterCommand::Ping => {
                client
                    .sitter_ping()
                    .send()
                    .await
                    .context("failed to call GET /ping (sitter)")?;
                println!("ok");
            }
            MahiSitterCommand::Snapshot { output } => {
                let resp = client
                    .sitter_snapshot()
                    .send()
                    .await
                    .context("failed to call GET /snapshot (sitter)")?;
                let stream = resp.into_inner();
                use futures_util::TryStreamExt;
                let mut stream = stream.into_inner();

                match output {
                    Some(path) => {
                        use tokio::io::AsyncWriteExt;
                        let mut file = tokio::fs::File::create(&path).await.with_context(|| {
                            format!("failed to open snapshot output file '{path}'")
                        })?;
                        let mut total: u64 = 0;
                        while let Some(chunk) = stream
                            .try_next()
                            .await
                            .context("failed reading snapshot stream")?
                        {
                            total += chunk.len() as u64;
                            file.write_all(&chunk)
                                .await
                                .context("failed writing snapshot chunk")?;
                        }
                        file.flush().await.context("failed to flush snapshot")?;
                        eprintln!("Wrote {total} bytes to {path}");
                    }
                    None => {
                        use std::io::Write;
                        let mut stdout = std::io::stdout().lock();
                        while let Some(chunk) = stream
                            .try_next()
                            .await
                            .context("failed reading snapshot stream")?
                        {
                            stdout
                                .write_all(&chunk)
                                .context("failed writing snapshot chunk to stdout")?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Print an `AuthInfo` either as raw JSON or a short summary.
fn print_auth_info(auth: &types::AuthInfo, raw: bool) -> Result<()> {
    if raw {
        println!("{}", serde_json::to_string_pretty(auth)?);
    } else {
        println!(
            "account: {} ({}) type={}",
            auth.account.login,
            auth.account.uuid,
            enum_to_display(&auth.account.type_)
        );
        if let Some(user) = &auth.user {
            println!(
                "user: {} ({}) type={}",
                user.login,
                user.uuid,
                enum_to_display(&user.type_)
            );
        }
        if !auth.roles.is_empty() {
            println!("roles: {}", auth.roles.len());
        }
        if let Some(role) = &auth.role {
            println!("role: {} ({})", role.name, role.uuid);
        }
    }
    Ok(())
}

/// Print an `IamRole` as a short summary.
fn print_iam_role(role: &types::IamRole) {
    println!("roleName: {}", role.role_name);
    println!("roleId: {}", role.role_id);
    println!("arn: {}", role.arn);
    println!("path: {}", role.path);
    println!("createDate: {}", role.create_date);
    if let Some(desc) = &role.description {
        println!("description: {desc}");
    }
    if let Some(msd) = role.max_session_duration {
        println!("maxSessionDuration: {msd}");
    }
}
