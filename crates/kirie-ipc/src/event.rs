use crossbeam_channel::Sender;

use crate::command::{Command, WorkshopRequest};
use crate::status::StatusSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Ok,
    Error,
}

#[derive(Debug)]
pub enum IpcEvent {
    Command {
        command: Command,
        reply: Sender<CommandOutcome>,
    },
    List {
        reply: Sender<String>,
    },
    Workshop {
        request: WorkshopRequest,
        reply: Sender<String>,
    },
    Status {
        reply: Sender<StatusSnapshot>,
    },
    GetProperties {
        screen: Option<String>,
        reply: Sender<String>,
    },
}
