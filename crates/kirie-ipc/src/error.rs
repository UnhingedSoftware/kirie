use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("failed to bind control socket at {path}: {source}")]
    Bind { path: PathBuf, source: io::Error },

    #[error("failed to spawn the control-socket thread: {0}")]
    Spawn(#[source] io::Error),
}
