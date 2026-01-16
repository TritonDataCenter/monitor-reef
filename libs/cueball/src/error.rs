// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

use thiserror::Error;

/// The cueball `Error` type is an `enum` that represents the different errors
/// that may be returned by the cueball API.
#[derive(Debug, Error)]
pub enum Error {
    /// The call to `claim` to failed to retrieve a connection within the
    /// specified timeout period.
    #[error("Unable to retrieve a connection within the claim timeout")]
    ClaimFailure,

    /// The `stop` function was called on a pool clone. Only the original
    /// connection pool instance may stop a connection pool. Thread `JoinHandles`
    /// may not be cloned and therefore invocation of this function by a clone
    /// of the pool results in an error.
    #[error("ConnectionPool clones may not stop the connection pool.")]
    StopCalledByClone,

    /// A backend key was found with no associated connection. This error should
    /// never happen and is only represented for completeness. Please file a bug
    /// if it is encountered.
    #[error("Found a backend key with no associated connection")]
    BackendWithNoConnection,

    /// A connection could not be retrieved from the connection pool even though
    /// the connection pool accounting indicated one should be available. This
    /// error should never happen and is only represented for
    /// completeness. Please file a bug if it is encountered.
    #[error("Unable to retrieve a connection")]
    ConnectionRetrievalFailure,

    // For internal pool use only
    #[doc(hidden)]
    #[error("dummy error")]
    DummyError,
}
