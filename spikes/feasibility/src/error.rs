use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum SpikeError {
    #[error("invalid spike invocation: {0}")]
    Invocation(String),

    #[error("required dependency `{name}` is unavailable: {detail}")]
    Dependency { name: &'static str, detail: String },

    #[error("failed to read `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("configuration `{path}` exceeds the {limit}-byte spike limit")]
    ConfigSize { path: PathBuf, limit: usize },

    #[error("configuration `{path}` exceeds the maximum nesting depth of {limit}")]
    ConfigDepth { path: PathBuf, limit: usize },

    #[error("configuration `{path}` violates a bounded-value limit at `{location}`: {detail}")]
    ConfigBound {
        path: PathBuf,
        location: String,
        detail: String,
    },

    #[error("invalid JSONC in `{path}`: {detail}")]
    Jsonc { path: PathBuf, detail: String },

    #[error("could not compile the pinned Dev Container schema: {0}")]
    SchemaCompile(String),

    #[error("`{path}` is not valid for cdenv-devcontainer-v1:\n{diagnostics}")]
    Schema { path: PathBuf, diagnostics: String },

    #[error("invalid cdenv-devcontainer-v1 profile at `{path}`: {detail}")]
    Profile { path: String, detail: String },

    #[error("Feature resolution failed: {0}")]
    Feature(String),

    #[error("Feature archive rejected: {0}")]
    Archive(String),

    #[error("OCI reference `{reference}` is unsupported: {detail}")]
    OciReference { reference: String, detail: String },

    #[error("OCI request to `{url}` failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("OCI response from `{url}` is invalid: {detail}")]
    OciResponse { url: String, detail: String },

    #[error("Docker endpoint is unsupported for V1: `{0}`")]
    DockerEndpoint(String),

    #[error("Docker API failed: {0}")]
    Docker(#[from] bollard::errors::Error),

    #[error("external command `{program}` could not start: {source}")]
    CommandStart {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("external command failed ({status}): {command}\n{stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },

    #[error("async task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub(crate) type Result<T> = std::result::Result<T, SpikeError>;

pub(crate) fn read(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| SpikeError::Read {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).map_err(|source| SpikeError::Write {
        path: path.to_path_buf(),
        source,
    })
}
