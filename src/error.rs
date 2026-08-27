// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Intervention(String),
    #[error("{0}")]
    NotFound(String),
    #[error("Experiment {id} failed (partial evidence retained): {source}")]
    ExperimentFailed { id: String, source: Box<Error> },
    #[error("Git operation failed: {0}")]
    Git(String),
    #[error("Could not start '{program}': {source}. Check the executable or --agent-command.")]
    ProcessStart {
        program: String,
        source: std::io::Error,
    },
    #[error("{source}; Reality {id} retained at {path} for inspection")]
    RealityPreserved {
        id: String,
        path: String,
        source: Box<Error>,
    },
    #[error("Filesystem/process error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{primary}; additionally, cleanup failed: {cleanup}")]
    Cleanup {
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Intervention(_) => 5,
            Self::RealityPreserved { source, .. } => source.exit_code(),
            Self::ExperimentFailed { source, .. } => source.exit_code(),
            _ => 2,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
