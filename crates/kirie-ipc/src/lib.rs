#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod os;
mod server;
mod status;

pub use command::{ClampMode, Command, Request, ScalingMode, SetOption, WorkshopRequest, parse_request};
pub use error::IpcError;
pub use event::{CommandOutcome, IpcEvent};
// Windows has AF_UNIX too; only the standard library's door to it is
// Unix-only. Everything in kirie that talks to the control socket goes through
// these rather than `std::os::unix::net`, so there is one place to change.
pub use os::{UnixListener, UnixStream, path_bytes, path_from_bytes};
pub use server::ControlSocket;
pub use status::{ScreenStatus, StatusSnapshot};
