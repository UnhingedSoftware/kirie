#![forbid(unsafe_code)]

pub mod error;
pub mod material;
pub mod object;
pub mod particle;
pub mod property;
pub mod resolve;
pub mod scene;
pub mod user;
pub mod value;

pub use error::SceneError;
pub use property::{PropertyBag, PropertyValue};
pub use resolve::{AssetProblem, AssetSource, SceneModel};
pub use scene::{Camera, General, Projection, Scene};
pub use user::UserSetting;
pub use value::{Color, DynamicValue, Vec2, Vec3};
