//! Socket-thread → app messages (SPEC V3: typed commands over a channel,
//! immutable snapshots back over per-request reply channels).

use crossbeam_channel::Sender;

use crate::command::{Command, WorkshopRequest};
use crate::status::StatusSnapshot;

/// App-side outcome of a fallible command, mapped onto the wire vocabulary
/// (docs/compat-socket.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Command applied → `ok\n`.
    Ok,
    /// Command understood but rejected/failed → `error\n`.
    Error,
}

/// One request delivered to the app. The app must reply **exactly once** on
/// the embedded channel; the socket thread blocks on that reply, which
/// reproduces the C++ engine's synchronous command ordering (doc §5) without
/// sharing any state (SPEC V3) and without ever involving the render thread
/// (SPEC V4).
///
/// Dropping the reply sender without replying (e.g. the app is shutting
/// down) makes the server close the connection with **zero response bytes**
/// — the protocol's dead-engine signal (doc §3).
#[derive(Debug)]
pub enum IpcEvent {
    /// A typed command (doc §4). For commands where
    /// [`Command::is_fallible`] is `false` (`speed`, `volume`, `mute`,
    /// `set`, `preload`) the C++ socket layer replies `ok\n` unconditionally
    /// (ControlSocket.cpp:100-132 per doc §4), so the outcome value is
    /// ignored there — but a reply is still required as the completion ack.
    Command {
        /// The parsed command.
        command: Command,
        /// Reply channel; `bounded(1)`, send never blocks.
        reply: Sender<CommandOutcome>,
    },
    /// `list` request (docs/compat-socket.md §12, a kirie extension): the app
    /// answers with the installed Wallpaper Engine items already serialized as
    /// a **single-line JSON array**. The server writes it verbatim plus one
    /// trailing `\n`, exactly as it does for `getproperties`.
    List {
        /// Reply channel; `bounded(1)`, send never blocks.
        reply: Sender<String>,
    },
    /// `workshop …` request (docs/compat-socket.md §13, a kirie extension):
    /// the app answers with a **single-line JSON** body, framed by the server
    /// with one trailing `\n` exactly as `list` is.
    ///
    /// Unlike every other event here, the app must **not** do the work before
    /// replying: a Workshop query talks to Steam through a helper process and
    /// takes seconds, while a subscription takes as long as the download. The
    /// app answers `search`/`state` from a worker thread and `subscribe` with
    /// a job id straight away, so neither the app loop nor the socket thread
    /// waits on Steam (SPEC V4).
    Workshop {
        /// Which Workshop request this is.
        request: WorkshopRequest,
        /// Reply channel carrying the JSON body; `bounded(1)`, send never
        /// blocks. Dropping it without a reply ⇒ zero response bytes (the
        /// dead-engine signal, doc §3).
        reply: Sender<String>,
    },
    /// `status` request (doc §4.2): the app answers with an immutable
    /// snapshot; the server does the byte formatting.
    Status {
        /// Reply channel; `bounded(1)`, send never blocks.
        reply: Sender<StatusSnapshot>,
    },
    /// `getproperties [screen]` read-back (docs/compat-socket.md §11, a kirie
    /// extension): the app answers with the selected screen's property schema
    /// already serialized as a **single-line JSON array** (post-override
    /// current values). The server writes it verbatim plus one trailing `\n`,
    /// so the payload must contain no embedded newline. A missing/unknown
    /// screen ⇒ the empty schema `"[]"`. Building the JSON app-side keeps
    /// `kirie-ipc` free of the `project.json`/schema dependency (SPEC V3: owned
    /// data crosses the channel).
    GetProperties {
        /// Screen to report, or `None` for the app's default screen.
        screen: Option<String>,
        /// Reply channel carrying the JSON array body; `bounded(1)`, send never
        /// blocks. Dropping it without a reply ⇒ zero response bytes (the
        /// dead-engine signal, doc §3).
        reply: Sender<String>,
    },
}
