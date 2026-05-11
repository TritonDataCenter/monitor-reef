// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Small accessors over [`crate::auth::Principal`] used pervasively by
//! the handler surface.

use dropshot::{ClientErrorStatusCode, HttpError};
use uuid::Uuid;

use crate::auth::Principal;

pub(crate) fn principal_silo_id(p: &Principal) -> Option<Uuid> {
    match p {
        Principal::Operator { silo_id, .. } => *silo_id,
        Principal::Anonymous => None,
    }
}

pub(crate) fn principal_tenant_id(p: &Principal) -> Option<Uuid> {
    match p {
        Principal::Operator { tenant_id, .. } => *tenant_id,
        Principal::Anonymous => None,
    }
}

pub(crate) fn principal_user_id(p: &Principal) -> Option<Uuid> {
    match p {
        Principal::Operator { user_id, .. } => Some(*user_id),
        Principal::Anonymous => None,
    }
}

/// Stable label for a principal in audit/window-tracking JSON.
/// Compact form so the audit blob stays single-line.
pub(crate) fn principal_label(principal: &Principal) -> String {
    match principal {
        Principal::Operator { user_id, .. } => user_id.to_string(),
        Principal::Anonymous => "anonymous".to_string(),
    }
}

/// 403 if the request didn't come from a bound API key. Used
/// by handlers that *only* make sense for a per-CN agent (the
/// heartbeat / status endpoints), since there's no other way
/// to know which CN to attribute the call to.
pub(crate) fn require_bound_cn(principal: &Principal) -> Result<Uuid, HttpError> {
    crate::auth::principal_bound_cn(principal).ok_or_else(|| {
        HttpError::for_client_error(
            Some("Forbidden".to_string()),
            ClientErrorStatusCode::FORBIDDEN,
            "this endpoint requires a CN-bound api key (the per-CN keys minted by /v2/cn-approvals)"
                .to_string(),
        )
    })
}
