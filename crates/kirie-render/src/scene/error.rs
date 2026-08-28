#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    #[error("scene has no renderable objects")]
    NoRenderableObjects,

    #[error("scene projection size is degenerate ({width}x{height})")]
    BadProjection { width: u32, height: u32 },
}
