// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Wire types for the tritond control plane.
//!
//! Phase 0 deliberately re-exports the storage-layer types from
//! [`tritond_store`] so that the wire schema and the persistence
//! schema cannot drift while the trait surface is still being shaped.
//! Once a wire type genuinely needs to differ from its stored form
//! (e.g. computed fields, redaction), define the wire-only type here
//! and convert in the service layer.

pub use tritond_audit::{
    Actor as AuditActor, AuditEvent, ChainHead as AuditChainHead, Decision as AuditDecision,
    EventHash as AuditEventHash, Outcome as AuditOutcome, VerifyOutcome as AuditVerifyOutcome,
};
pub use tritond_store::{
    ApiKeyView, IdpConfigView, Image, NewImage, NewProject, NewQuota, NewSilo, NewSshKey,
    NewSubnet, NewVpc, Project, Quota, Silo, SshKey, Subnet, UserView, Vpc,
};
