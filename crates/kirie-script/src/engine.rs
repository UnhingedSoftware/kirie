use std::thread::JoinHandle;

use crossbeam_channel::{Sender, bounded};

use crate::error::ScriptError;
use crate::frame::{HostFrame, TickOutput};
use crate::value::ScriptValue;
use crate::world::World;

pub const API_VERSION: &str = "2.8";

pub const TRANSLATOR_VERSION: u32 = 1;

const QUEUE_DEPTH: usize = 64;

enum Command {
    Load {
        key: String,
        source: String,
        owner_id: Option<i64>,
        initial: ScriptValue,
        script_properties: serde_json::Value,
        reply: Sender<Result<(), ScriptError>>,
    },
    Tick {
        frame: Box<HostFrame>,
        overrides: Vec<(String, ScriptValue)>,
        reply: Sender<(TickOutput, Box<HostFrame>)>,
    },
    DispatchUserProperty {
        key: String,
        value: ScriptValue,
        reply: Sender<TickOutput>,
    },
    Eval {
        source: String,
        reply: Sender<Result<String, ScriptError>>,
    },
    SetStoragePath {
        path: std::path::PathBuf,
    },
}

pub struct ScriptEngine {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ScriptEngine {
    pub fn new() -> Result<Self, ScriptError> {
        let (tx, rx) = bounded::<Command>(QUEUE_DEPTH);
        let (ready_tx, ready_rx) = bounded::<Result<(), ScriptError>>(1);
        let thread = std::thread::Builder::new()
            .name("kirie-script".into())
            .spawn(move || {
                let mut world = match World::new() {
                    Ok(w) => {
                        let _ = ready_tx.send(Ok(()));
                        w
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while let Ok(cmd) = rx.recv() {
                    serve(&mut world, cmd);
                }
            })
            .map_err(|e| ScriptError::Internal(format!("spawn script thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(ScriptEngine {
                tx,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ScriptError::ThreadGone),
        }
    }

    pub fn load_property_script(
        &self,
        key: impl Into<String>,
        source: impl Into<String>,
        owner_id: Option<i64>,
        initial: ScriptValue,
        script_properties: serde_json::Value,
    ) -> Result<(), ScriptError> {
        let (reply, rx) = bounded(1);
        self.send(Command::Load {
            key: key.into(),
            source: source.into(),
            owner_id,
            initial,
            script_properties,
            reply,
        })?;
        rx.recv().map_err(|_| ScriptError::ThreadGone)?
    }

    pub fn tick(
        &self,
        frame: HostFrame,
        overrides: Vec<(String, ScriptValue)>,
    ) -> Result<TickOutput, ScriptError> {
        self.tick_reuse(Box::new(frame), overrides).map(|(out, _)| out)
    }

    pub fn tick_reuse(
        &self,
        frame: Box<HostFrame>,
        overrides: Vec<(String, ScriptValue)>,
    ) -> Result<(TickOutput, Box<HostFrame>), ScriptError> {
        let (reply, rx) = bounded(1);
        self.send(Command::Tick {
            frame,
            overrides,
            reply,
        })?;
        rx.recv().map_err(|_| ScriptError::ThreadGone)
    }

    pub fn dispatch_user_property(
        &self,
        key: impl Into<String>,
        value: ScriptValue,
    ) -> Result<TickOutput, ScriptError> {
        let (reply, rx) = bounded(1);
        self.send(Command::DispatchUserProperty {
            key: key.into(),
            value,
            reply,
        })?;
        rx.recv().map_err(|_| ScriptError::ThreadGone)
    }

    pub fn set_storage_path(&self, path: std::path::PathBuf) -> Result<(), ScriptError> {
        self.send(Command::SetStoragePath { path })
    }

    pub fn eval(&self, source: impl Into<String>) -> Result<String, ScriptError> {
        let (reply, rx) = bounded(1);
        self.send(Command::Eval {
            source: source.into(),
            reply,
        })?;
        rx.recv().map_err(|_| ScriptError::ThreadGone)?
    }

    fn send(&self, cmd: Command) -> Result<(), ScriptError> {
        self.tx.send(cmd).map_err(|_| ScriptError::ThreadGone)
    }
}

impl Drop for ScriptEngine {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            drop(std::mem::replace(&mut self.tx, bounded(0).0));
            let _ = thread.join();
        }
    }
}

fn serve(world: &mut World, cmd: Command) {
    match cmd {
        Command::Load {
            key,
            source,
            owner_id,
            initial,
            script_properties,
            reply,
        } => {
            let r = world.load_property_script(&key, &source, owner_id, initial, &script_properties);
            let _ = reply.send(r);
        }
        Command::Tick {
            frame,
            overrides,
            reply,
        } => {
            let out = world.tick(&frame, &overrides);
            let _ = reply.send((out, frame));
        }
        Command::DispatchUserProperty { key, value, reply } => {
            let _ = reply.send(world.dispatch_user_property(&key, &value));
        }
        Command::Eval { source, reply } => {
            let _ = reply.send(world.eval_to_string(&source));
        }
        Command::SetStoragePath { path } => {
            world.set_storage_path(path);
        }
    }
}
