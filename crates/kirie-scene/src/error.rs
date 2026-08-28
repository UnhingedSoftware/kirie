use thiserror::Error;

use crate::value::VecError;

#[derive(Debug, Error, PartialEq)]
pub enum SceneError {
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("scene.json root is not a JSON object")]
    NotAnObject,
    #[error("required section `{0}` missing")]
    MissingSection(&'static str),
    #[error("camera field `{0}` missing")]
    MissingCameraField(&'static str),
    #[error("camera field `{field}`: {source}")]
    CameraVec { field: &'static str, source: VecError },
}
