/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 * Copyright 2025 Edgecast Cloud LLC.
 */

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Internal(InternalError),
    IoError(std::io::Error),
    Hyper(hyper::Error),
    Diesel(diesel::result::Error),
    SerdeJson(serde_json::error::Error),
    Reqwest(reqwest::Error),
    ParseInt(std::num::ParseIntError),
    ParseUuid(uuid::parser::ParseError),
    DieselConnection(diesel::ConnectionError),
    Mdapi(libmanta::mdapi::MdapiError),
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        match self {
            Error::Internal(e) => e.msg.as_str(),
            Error::IoError(e) => e.description(),
            Error::Hyper(e) => e.description(),
            Error::Diesel(e) => e.description(),
            Error::SerdeJson(e) => e.description(),
            Error::Reqwest(e) => e.description(),
            Error::ParseInt(e) => e.description(),
            Error::ParseUuid(e) => e.description(),
            Error::DieselConnection(e) => e.description(),
            Error::Mdapi(e) => match e {
                libmanta::mdapi::MdapiError::BucketAlreadyExists(_) => "Bucket already exists",
                libmanta::mdapi::MdapiError::BucketNotFound(_) => "Bucket not found",
                libmanta::mdapi::MdapiError::ObjectNotFound(_) => "Object not found",
                libmanta::mdapi::MdapiError::InvalidLimit(_) => "Invalid limit",
                libmanta::mdapi::MdapiError::PreconditionFailed(_) => "Precondition failed",
                libmanta::mdapi::MdapiError::DatabaseError(_) => "Database error",
                libmanta::mdapi::MdapiError::InvalidContentMd5(_) => "Invalid content MD5",
                libmanta::mdapi::MdapiError::RpcError(_) => "RPC error",
                libmanta::mdapi::MdapiError::SerializationError(_) => "Serialization error",
                libmanta::mdapi::MdapiError::IoError(_) => "IO error",
                libmanta::mdapi::MdapiError::Other(_) => "Other error",
            },
        }
    }
}

impl From<hyper::Error> for Error {
    fn from(error: hyper::Error) -> Self {
        Error::Hyper(error)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::IoError(error)
    }
}

impl From<InternalError> for Error {
    fn from(error: InternalError) -> Self {
        Error::Internal(error)
    }
}

impl From<diesel::result::Error> for Error {
    fn from(error: diesel::result::Error) -> Self {
        Error::Diesel(error)
    }
}

impl From<serde_json::error::Error> for Error {
    fn from(error: serde_json::error::Error) -> Self {
        Error::SerdeJson(error)
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Error::Reqwest(error)
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(error: std::num::ParseIntError) -> Self {
        Error::ParseInt(error)
    }
}

impl From<uuid::parser::ParseError> for Error {
    fn from(error: uuid::parser::ParseError) -> Self {
        Error::ParseUuid(error)
    }
}

impl From<diesel::ConnectionError> for Error {
    fn from(error: diesel::ConnectionError) -> Self {
        Error::DieselConnection(error)
    }
}

impl From<libmanta::mdapi::MdapiError> for Error {
    fn from(error: libmanta::mdapi::MdapiError) -> Self {
        Error::Mdapi(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Internal(e) => write!(f, "{}", e),
            Error::IoError(e) => write!(f, "{}", e),
            Error::Hyper(e) => write!(f, "{}", e),
            Error::Diesel(e) => write!(f, "{}", e),
            Error::SerdeJson(e) => write!(f, "{}", e),
            Error::Reqwest(e) => write!(f, "{}", e),
            Error::ParseInt(e) => write!(f, "{}", e),
            Error::ParseUuid(e) => write!(f, "{}", e),
            Error::DieselConnection(e) => write!(f, "{}", e),
            Error::Mdapi(e) => write!(f, "Mdapi error: {:?}", e),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalError {
    msg: String,
    pub code: InternalErrorCode,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum InternalErrorCode {
    Other,
    Crossbeam,             // An error relating to crossbeam
    StorinfoError,         // An error from Storinfo service
    AssignmentLookupError, // Could not lookup assignment in memory
    AssignmentGetError,    // Could not get assignment from agent
    IpLookupError,         // Could not lookup IP in DNS
    SharkNotFound,         // Could not find shark
    DuplicateShark,        // Found the same shark twice in object metadata
    BadMantaObject,        // Manta object is malformed is missing data
    BadMorayClient,        // Moray client errors
    BadMdapiClient,        // Mdapi client errors
    MetadataUpdateFailure, // Errors updating metadata in moray or mdapi
    MdapiBucketNotFound,   // Bucket not found in mdapi
    MdapiObjectNotFound,   // Object not found in mdapi
    JobBuilderError,       // Errors building a Job
    MaxObjectsLimit,       // The max_objects limit has been reached
    DbQuery,               // Unexpected result from a database query
}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let code = self.code as i32;
        write!(f, "Error {}: {}", code, self.msg)
    }
}

impl InternalError {
    pub fn new<S: Into<String>>(
        err_code: Option<InternalErrorCode>,
        message: S,
    ) -> Self {
        let mut code = InternalErrorCode::Other;
        let msg = message.into();

        if let Some(c) = err_code {
            code = c;
        }

        InternalError { msg, code }
    }
}

#[derive(Debug)]
pub enum CrossbeamError<T> {
    Send(crossbeam_channel::SendError<T>),
    Recv(crossbeam_channel::RecvError),
}

impl<T> From<crossbeam_channel::SendError<T>> for CrossbeamError<T> {
    fn from(error: crossbeam_channel::SendError<T>) -> Self {
        CrossbeamError::Send(error)
    }
}

impl<T> From<crossbeam_channel::RecvError> for CrossbeamError<T> {
    fn from(error: crossbeam_channel::RecvError) -> Self {
        CrossbeamError::Recv(error)
    }
}

impl<T> std::error::Error for CrossbeamError<T>
where
    T: std::fmt::Debug + Send,
{
    fn description(&self) -> &str {
        match self {
            CrossbeamError::Send(e) => e.description(),
            CrossbeamError::Recv(e) => e.description(),
        }
    }
}

impl<T> fmt::Display for CrossbeamError<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CrossbeamError::Send(e) => write!(f, "{}", e),
            CrossbeamError::Recv(e) => write!(f, "{}", e),
        }
    }
}
