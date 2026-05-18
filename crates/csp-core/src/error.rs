//! Engine error type. Every fallible engine call returns [`CspResult`].

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CspError {
    #[error("object {0} not found")]
    ObjectNotFound(String),
    #[error("malformed git object: {0}")]
    Malformed(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("signature verification failed: {0}")]
    BadSignature(String),
    #[error("unauthorized author: {0}")]
    Unauthorized(String),
    #[error("fold verification failed: {0}")]
    FoldVerify(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("scope violation: {0}")]
    Scope(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for CspError {
    fn from(e: std::io::Error) -> Self {
        CspError::Io(e.to_string())
    }
}

pub type CspResult<T> = Result<T, CspError>;
