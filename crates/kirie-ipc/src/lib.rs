#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod server;
mod status;

pub use command::{ClampMode, Command, Request, ScalingMode, SetOption, WorkshopRequest, parse_request};
pub use error::IpcError;
pub use event::{CommandOutcome, IpcEvent};
pub use server::ControlSocket;
pub use status::{ScreenStatus, StatusSnapshot};
