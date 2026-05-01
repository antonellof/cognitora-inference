//! Workspace-wide error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("config: {0}")]
    Config(String),

    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("tls: {0}")]
    Tls(String),

    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("rpc: {0}")]
    Status(#[from] tonic::Status),

    #[error("etcd: {0}")]
    Etcd(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unavailable: {0}")]
    Unavailable(String),

    #[error("internal: {0}")]
    Internal(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<Error> for tonic::Status {
    fn from(e: Error) -> Self {
        use tonic::Code;
        let code = match &e {
            Error::InvalidArgument(_) => Code::InvalidArgument,
            Error::NotFound(_) => Code::NotFound,
            Error::Unavailable(_) => Code::Unavailable,
            Error::Status(s) => return s.clone(),
            _ => Code::Internal,
        };
        tonic::Status::new(code, e.to_string())
    }
}
