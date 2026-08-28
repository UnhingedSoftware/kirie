use thiserror::Error;

#[derive(Clone, Debug, Error)]
pub enum ScriptError {
    #[error("script {key:?} failed to load: {message}")]
    Load { key: String, message: String },

    #[error("script {key:?} threw in {phase}: {message}")]
    Runtime {
        key: String,
        phase: &'static str,
        message: String,
    },

    #[error("script engine thread is not running")]
    ThreadGone,

    #[error("internal script error: {0}")]
    Internal(String),
}
