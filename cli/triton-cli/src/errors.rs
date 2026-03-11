// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

/// Error type for resource-not-found conditions.
///
/// Commands that fail because a named resource (instance, image, package, etc.)
/// cannot be resolved should return this error so that `main()` can exit with
/// code 3, matching the Node.js `triton` CLI convention.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ResourceNotFoundError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_not_found_downcast() {
        let err: anyhow::Error =
            ResourceNotFoundError("Instance not found: foo".to_string()).into();
        assert!(err.downcast_ref::<ResourceNotFoundError>().is_some());
        assert_eq!(err.to_string(), "Instance not found: foo");
    }

    #[test]
    fn other_errors_do_not_downcast_as_not_found() {
        let err = anyhow::anyhow!("connection refused");
        assert!(err.downcast_ref::<ResourceNotFoundError>().is_none());
    }
}
