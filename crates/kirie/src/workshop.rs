//! `kirie workshop` — browse the Steam Workshop, not just what is installed.
//!
//! [`crate::list`] answers "what do I have"; this answers "what is there". The
//! two report the same keys (`id`, `title`, `type`, `preview`, `renderable`)
//! so a picker can show installed and browsable wallpapers side by side, with
//! `dir: null` marking the ones whose files have not arrived yet.
//!
//! kirie never links Steam. Every call here spawns `kirie-steam-helper`, which
//! dlopens the user's own Steam client, answers one request as JSON and exits
//! — see that crate's docs for why the connection must not be held open, and
//! SPEC §V2 for why the unsafe lives there instead of here.
//!
//! What kirie adds on top of Steam's own answer is the thing Steam cannot say:
//! whether *this build* can render an item, decided from the item's type by
//! the same match the engine uses on load.

#[cfg(feature = "tui")]
pub mod tui;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::compat::resolve;

/// The most results Steam returns for one query, and so the most this will
/// parse out of a helper answer (V9: a wrong count must not drive a big
/// allocation).
const PAGE_SIZE: usize = 50;

/// One Workshop item as a search returned it.
#[derive(Debug, Clone)]
pub struct Item {
    /// Workshop id.
    pub id: String,
    /// The item's Workshop title.
    pub title: String,
    /// `scene` / `video` / `web` / `application` / `unknown`, from the type tag
    /// Steam carries on every item.
    pub kind: &'static str,
    /// Where the item is installed, when it already is.
    pub dir: Option<PathBuf>,
    /// The Workshop preview image — a URL here, where an installed item has a
    /// local path.
    pub preview: Option<String>,
    /// Whether **this build** can render it. Decided from `kind` alone, since
    /// there are no files to inspect until it is installed.
    pub renderable: bool,
    /// Why not, when `renderable` is false.
    pub reason: Option<String>,
    /// Whether this account is subscribed to it.
    pub subscribed: bool,
    /// Whether its files are on disk.
    pub installed: bool,
    /// Bytes, as Steam reports them.
    pub size: u64,
    /// Upvotes and downvotes.
    pub votes: (u32, u32),
    /// Steam's own 0..=1 score.
    pub score: f32,
    /// When the item was last updated, as a Unix timestamp.
    pub updated: u32,
    /// The item's Workshop tags, verbatim.
    pub tags: Vec<String>,
}

/// What to ask the Workshop for.
#[derive(Debug, Default, Clone)]
pub struct Query {
    /// Free-text search.
    pub text: Option<String>,
    /// Tags an item must all carry (or any of, with `match_any_tag`).
    pub tags: Vec<String>,
    /// Tags that disqualify an item.
    pub excluded_tags: Vec<String>,
    /// Match any required tag rather than all of them.
    pub match_any_tag: bool,
    /// `popular` / `trend` / `recent` / `rated`.
    pub sort: String,
    /// The window `--sort trend` ranks over.
    pub trend_days: Option<u32>,
    /// 1-based page.
    pub page: u32,
    /// Keep at most this many results of the page.
    pub limit: Option<usize>,
}

/// Run one Workshop query.
///
/// # Errors
/// When the helper is missing, Steam is not running, the account does not own
/// Wallpaper Engine, or Steam refuses the query — each with the helper's own
/// message, which says which of those it was.
pub fn search(query: &Query) -> Result<Vec<Item>> {
    let mut args: Vec<String> = vec!["search".to_owned()];
    if let Some(text) = &query.text {
        args.push(format!("text={text}"));
    }
    for tag in &query.tags {
        args.push(format!("tag={tag}"));
    }
    for tag in &query.excluded_tags {
        args.push(format!("nottag={tag}"));
    }
    if query.match_any_tag {
        args.push("anytag=1".to_owned());
    }
    if !query.sort.is_empty() {
        args.push(format!("sort={}", query.sort));
    }
    if let Some(days) = query.trend_days {
        args.push(format!("days={days}"));
    }
    args.push(format!("page={}", query.page.max(1)));

    let answer = ask_helper(&args)?;
    let array = answer
        .as_array()
        .ok_or_else(|| anyhow!("the Steam helper did not return a list of results"))?;

    let mut items: Vec<Item> = array
        .iter()
        .take(PAGE_SIZE)
        .filter_map(|value| item_from(value).ok())
        .collect();
    if let Some(limit) = query.limit {
        items.truncate(limit);
    }
    mark_local(&mut items);
    Ok(items)
}

/// Fill in what this machine already has.
///
/// A Workshop query answers for the item, not for the account: Steam's search
/// results carry no "you have this" flag. The answer is on disk anyway — the
/// workshop content directories and Steam's own bookkeeping — and reading it
/// here costs one directory listing rather than a round trip per result, and
/// works with the client closed.
fn mark_local(items: &mut [Item]) {
    if items.is_empty() {
        return;
    }

    let installed: std::collections::HashMap<String, PathBuf> = resolve::workshop_dirs()
        .into_iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .collect();
    let subscribed: std::collections::HashSet<String> =
        crate::compat::steam::workshop_item_states(crate::compat::args::WORKSHOP_APP_ID)
            .into_iter()
            .filter(|state| state.subscribed)
            .map(|state| state.id)
            .collect();

    for item in items {
        if let Some(dir) = installed.get(&item.id) {
            item.installed = true;
            item.dir = Some(dir.clone());
            // Steam's tags are a guess about the files; the files themselves
            // are not. Once an item is on disk, classify it the way the engine
            // will when it loads it — which is also the only way `state` and
            // `subscribe`, which get no tags back from Steam at all, can say
            // anything about the wallpaper beyond its id.
            if let Some(local) = crate::list::describe(dir) {
                item.kind = local.kind;
                item.renderable = local.renderable;
                item.reason = local.reason;
                if item.title == item.id {
                    item.title = local.title;
                }
            }
        }
        if subscribed.contains(&item.id) {
            item.subscribed = true;
        }
    }
}

/// Subscribe to one item, so Steam downloads it into the workshop directory
/// kirie already reads.
///
/// Returns as soon as Steam accepts; the files land later. Callers that need
/// them wait by watching the directory, never by asking Steam again — see the
/// helper's docs.
///
/// # Errors
/// As [`search`], plus Steam refusing the subscription itself.
pub fn subscribe(id: &str) -> Result<Item> {
    let answer = ask_helper(&["subscribe".to_owned(), id.to_owned()])?;
    let mut item = item_from(&answer)?;
    mark_local(std::slice::from_mut(&mut item));
    Ok(item)
}

/// Report what Steam knows about one item without changing anything.
///
/// # Errors
/// As [`search`].
pub fn state(id: &str) -> Result<Item> {
    let answer = ask_helper(&["state".to_owned(), id.to_owned()])?;
    let mut item = item_from(&answer)?;
    mark_local(std::slice::from_mut(&mut item));
    Ok(item)
}

/// Wait for Steam to finish downloading an item, reporting progress.
///
/// Deliberately filesystem-only. Asking Steam how a download is going means
/// initialising Steamworks again, and every one of those announces the process
/// as *playing Wallpaper Engine* and accrues playtime (measured — see the
/// helper's module docs). Steam's own bookkeeping says the same thing for
/// free: an item appears under `WorkshopItemsInstalled` when its files are
/// complete, and the partial download has a directory whose size grows.
///
/// `on_progress` is called with the bytes fetched so far, about twice a
/// second, and only while the number changes.
///
/// # Errors
/// When the item has not arrived within `timeout`.
pub fn wait_for_install(
    id: &str,
    timeout: std::time::Duration,
    mut on_progress: impl FnMut(u64),
) -> Result<PathBuf> {
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);

    let started = std::time::Instant::now();
    let mut last_reported = u64::MAX;
    loop {
        if let Some(dir) = installed_dir(id) {
            return Ok(dir);
        }
        let fetched = partial_bytes(id);
        if fetched != last_reported {
            last_reported = fetched;
            on_progress(fetched);
        }
        if started.elapsed() > timeout {
            return Err(anyhow!(
                "Steam has not finished downloading {id} after {}s (it keeps going in the \
                 background; `kirie workshop state {id}` will say when it lands)",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Where an item's finished files are, if Steam says they are finished.
///
/// Both halves are required: the directory appears while the download is still
/// running, and Steam's record is what marks it complete.
fn installed_dir(id: &str) -> Option<PathBuf> {
    let complete = crate::compat::steam::workshop_item_states(crate::compat::args::WORKSHOP_APP_ID)
        .into_iter()
        .any(|state| state.id == id && state.installed);
    if !complete {
        return None;
    }
    resolve::workshop_dirs()
        .into_iter()
        .map(|root| root.join(id))
        .find(|dir| dir.join("project.json").is_file())
}

/// Bytes Steam has fetched for an item that is still downloading.
///
/// Steam stages a download under `steamapps/workshop/downloads/<app>/<id>` and
/// moves it into `content` when it is done, so the staging directory's size is
/// the progress. Zero before the transfer starts.
fn partial_bytes(id: &str) -> u64 {
    fn dir_size(dir: &std::path::Path, depth: u32) -> u64 {
        if depth == 0 {
            return 0;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|entry| match entry.metadata() {
                Ok(meta) if meta.is_dir() => dir_size(&entry.path(), depth - 1),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            })
            .sum()
    }

    let app = crate::compat::args::WORKSHOP_APP_ID.to_string();
    crate::compat::steam::steamapps_dirs(Path::new("workshop/downloads").join(&app))
        .into_iter()
        .map(|root| dir_size(&root.join(id), 8))
        .sum()
}

/// Point a running engine at a wallpaper, over its control socket.
///
/// The socket is the engine's own `bg <screen> <path>` verb
/// (docs/compat-socket.md §4), so this works against kirie and against the
/// reference engine alike.
///
/// # Errors
/// When no engine is listening on `socket`, or it refuses the command.
pub fn apply(socket: &Path, screen: &str, dir: &Path) -> Result<()> {
    use std::io::{Read, Write};

    let mut stream = std::os::unix::net::UnixStream::connect(socket).with_context(|| {
        format!(
            "no wallpaper engine is listening on {} (start kirie with --control-socket)",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;

    let mut request = Vec::new();
    request.extend_from_slice(b"bg ");
    request.extend_from_slice(screen.as_bytes());
    request.push(b' ');
    request.extend_from_slice(dir.as_os_str().as_encoded_bytes());
    request.push(b'\n');
    stream.write_all(&request)?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    if reply.trim() == "ok" {
        return Ok(());
    }
    Err(anyhow!(
        "the engine refused to show it: {}",
        reply.trim().to_owned()
    ))
}

/// Run the helper with these arguments plus every Steam library root, and
/// parse its single line of JSON.
///
/// The helper reports failure as `{"error": "…"}` on stdout rather than by
/// exit code alone, so that a caller always has something to show the user.
fn ask_helper(args: &[String]) -> Result<serde_json::Value> {
    let helper = helper_path().ok_or_else(|| {
        anyhow!(
            "kirie-steam-helper was not found (it ships beside kirie; set \
             KIRIE_STEAM_HELPER to point at it)"
        )
    })?;

    let mut command = std::process::Command::new(&helper);
    command.args(args);
    for root in crate::compat::steam::libraries() {
        command.arg(root);
    }
    // The Steam client writes its own diagnostics to stderr; only stdout is
    // the answer, and it is one line.
    command.stderr(std::process::Stdio::null());

    let output = command
        .output()
        .with_context(|| format!("could not run {}", helper.display()))?;
    // The answer is the LAST line, not the whole of stdout: the Steam client
    // library the helper dlopens prints banners of its own when it starts up,
    // and a client still coming up put one in front of the reply — which read
    // as "the helper did not answer with JSON" for as long as Steam took to
    // finish launching.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let answer = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(answer)
        .with_context(|| "the Steam helper did not answer with JSON".to_owned())?;

    if let Some(message) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(anyhow!(message.to_owned()));
    }
    Ok(value)
}

/// Whether Steam is reachable and this account owns Wallpaper Engine.
///
/// `Ok(false)` is the ownership answer; `Err` is everything that stopped us
/// asking (no Steam running, a client too old to carry the library), already
/// phrased for a user to read.
///
/// # Errors
/// When the helper could not ask Steam at all.
pub fn probe() -> Result<bool> {
    let answer = ask_helper(&["probe".to_owned()])?;
    // A refusal that is *about Steam* comes back as a normal answer with a
    // reason, not as `error` — see the helper's `probe`.
    if let Some(detail) = answer.get("detail").and_then(serde_json::Value::as_str) {
        return Err(anyhow!(detail.to_owned()));
    }
    Ok(answer
        .get("owned")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

/// Where the helper binary is.
///
/// `KIRIE_STEAM_HELPER` first (a packaging override, and what the tests use),
/// then beside kirie itself — which is how it ships — and finally `PATH`, for
/// a distribution that splits the two.
#[must_use]
pub fn helper_path() -> Option<PathBuf> {
    const NAME: &str = "kirie-steam-helper";

    if let Some(explicit) = std::env::var_os("KIRIE_STEAM_HELPER") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join(NAME);
        if beside.is_file() {
            return Some(beside);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(NAME))
        .find(|candidate| candidate.is_file())
}

/// Build an [`Item`] from one helper result.
///
/// The helper's JSON is untrusted input like any other (V9): every field is
/// optional here, and a result missing its id is dropped rather than guessed
/// at.
fn item_from(value: &serde_json::Value) -> Result<Item> {
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("a Workshop result had no id"))?
        .to_owned();

    let tags: Vec<String> = value
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .take(64)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let kind = kind_from_tags(&tags);
    let reason = resolve::reason_for_kind(kind);

    let string = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    };
    let number = |key: &str| value.get(key).and_then(serde_json::Value::as_u64);
    let flag = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };

    Ok(Item {
        title: string("title").unwrap_or_else(|| id.clone()),
        kind,
        dir: string("dir").map(PathBuf::from),
        preview: string("preview_url"),
        renderable: reason.is_none(),
        reason,
        subscribed: flag("subscribed"),
        installed: flag("installed"),
        size: number("size").unwrap_or(0),
        votes: (
            u32::try_from(number("votes_up").unwrap_or(0)).unwrap_or(u32::MAX),
            u32::try_from(number("votes_down").unwrap_or(0)).unwrap_or(u32::MAX),
        ),
        score: value
            .get("score")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32,
        updated: u32::try_from(number("updated").unwrap_or(0)).unwrap_or(u32::MAX),
        tags,
        id,
    })
}

/// The wallpaper type Steam's tags declare.
///
/// Every Workshop item carries exactly one type tag; the rest describe its
/// content. `Application` is matched first because an application item also
/// carries `Wallpaper`.
fn kind_from_tags(tags: &[String]) -> &'static str {
    let has = |name: &str| tags.iter().any(|tag| tag.eq_ignore_ascii_case(name));
    if has("Application") {
        "application"
    } else if has("Scene") {
        "scene"
    } else if has("Video") {
        "video"
    } else if has("Web") {
        "web"
    } else if has("Preset") || has("Asset") {
        "asset"
    } else {
        "unknown"
    }
}

/// The listing as a JSON array, sharing `kirie list`'s key names.
#[must_use]
pub fn to_json(items: &[Item]) -> String {
    let values: Vec<serde_json::Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "id": i.id,
                "title": i.title,
                "type": i.kind,
                "dir": i.dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
                "preview": i.preview,
                "renderable": i.renderable,
                "reason": i.reason,
                "subscribed": i.subscribed,
                "installed": i.installed,
                "size": i.size,
                "votes_up": i.votes.0,
                "votes_down": i.votes.1,
                "score": i.score,
                "updated": i.updated,
                "tags": i.tags,
            })
        })
        .collect();
    serde_json::Value::Array(values).to_string()
}

/// A subscription in flight, as `workshop job <n>` reports it.
///
/// The states are the ones a shell shows: Steam accepted it, the files are
/// coming, they arrived, or it went wrong.
#[derive(Debug, Clone)]
pub struct Job {
    /// The Workshop id being fetched.
    pub id: String,
    /// `subscribing` / `downloading` / `installed` / `error`.
    pub state: &'static str,
    /// Bytes fetched so far, while downloading.
    pub bytes: u64,
    /// Where the files landed, once they did.
    pub dir: Option<PathBuf>,
    /// What went wrong, when `state` is `error`.
    pub error: Option<String>,
}

impl Job {
    /// The job as the socket reports it — one line, no embedded newline.
    #[must_use]
    fn to_json(&self, job: u64) -> String {
        serde_json::json!({
            "job": job,
            "id": self.id,
            "state": self.state,
            "bytes": self.bytes,
            "dir": self.dir.as_ref().map(|d| d.to_string_lossy().into_owned()),
            "error": self.error,
        })
        .to_string()
    }
}

/// Subscriptions started over the control socket.
///
/// A download outlives the request that started it — the socket has no event
/// stream and clients time out in seconds (docs/compat-socket.md §6) — so the
/// subscription answers with a job id and the progress is left here for
/// whoever asks next.
#[derive(Debug, Default)]
pub struct Jobs {
    inner: std::sync::Mutex<JobTable>,
}

/// The job table itself: a counter and the jobs it has handed out.
#[derive(Debug, Default)]
struct JobTable {
    next: u64,
    jobs: std::collections::HashMap<u64, Job>,
}

impl Jobs {
    /// Record a new job for an id, returning its number.
    fn start(&self, id: &str) -> u64 {
        let Ok(mut table) = self.inner.lock() else {
            return 0;
        };
        table.next += 1;
        let number = table.next;
        // Keep the table from growing without bound over a long-lived daemon:
        // a finished job is only interesting until someone reads it, and 64 is
        // far more than any shell has in flight.
        if table.jobs.len() >= 64
            && let Some(oldest) = table
                .jobs
                .iter()
                .filter(|(_, job)| job.state == "installed" || job.state == "error")
                .map(|(n, _)| *n)
                .min()
        {
            table.jobs.remove(&oldest);
        }
        table.jobs.insert(
            number,
            Job {
                id: id.to_owned(),
                state: "subscribing",
                bytes: 0,
                dir: None,
                error: None,
            },
        );
        number
    }

    /// Update one job in place. A poisoned lock is dropped silently: losing
    /// progress is better than taking the daemon down with it (V9).
    fn update(&self, number: u64, edit: impl FnOnce(&mut Job)) {
        if let Ok(mut table) = self.inner.lock()
            && let Some(job) = table.jobs.get_mut(&number)
        {
            edit(job);
        }
    }

    /// One job, if it exists.
    fn get(&self, number: u64) -> Option<Job> {
        self.inner.lock().ok()?.jobs.get(&number).cloned()
    }
}

/// Parse a `workshop search` argument line (`key=value`, space-separated).
///
/// `text=` takes the rest of the line, since a search phrase has spaces in it;
/// everything before it is a filter. An unknown key is ignored rather than
/// refused — a newer shell talking to an older engine should still search.
#[must_use]
pub fn query_from_args(line: &str) -> Query {
    let mut query = Query {
        sort: "popular".to_owned(),
        page: 1,
        ..Query::default()
    };

    let mut rest = line.trim();
    while !rest.is_empty() {
        let (token, tail) = match rest.split_once(char::is_whitespace) {
            Some((token, tail)) => (token, tail.trim_start()),
            None => (rest, ""),
        };
        let Some((key, value)) = token.split_once('=') else {
            rest = tail;
            continue;
        };
        match key {
            // Everything after `text=` is the phrase, tail included.
            "text" => {
                let phrase = if tail.is_empty() {
                    value.to_owned()
                } else {
                    format!("{value} {tail}")
                };
                query.text = Some(phrase.trim().to_owned());
                return query;
            }
            "tag" => query.tags.push(value.to_owned()),
            "nottag" => query.excluded_tags.push(value.to_owned()),
            "anytag" => query.match_any_tag = value != "0",
            "sort" => query.sort = value.to_owned(),
            "days" => query.trend_days = value.parse().ok(),
            "page" => query.page = value.parse().unwrap_or(1),
            "limit" => query.limit = value.parse().ok(),
            _ => {}
        }
        rest = tail;
    }
    query
}

/// Answer one control-socket `workshop` request.
///
/// Never blocks the caller: every verb that talks to Steam is run on its own
/// thread and answers through `reply`, because the app loop it is called from
/// also drives wallpaper swaps (SPEC V4). `subscribe` answers immediately with
/// a job number and follows the download in the background.
pub fn serve_socket(
    jobs: &std::sync::Arc<Jobs>,
    request: kirie_ipc::WorkshopRequest,
    reply: crossbeam_channel::Sender<String>,
) {
    use kirie_ipc::WorkshopRequest as W;

    /// An error the shell can show, in the same one-line JSON shape as a
    /// result.
    fn error(message: &str) -> String {
        serde_json::json!({ "error": message }).to_string()
    }

    let jobs = std::sync::Arc::clone(jobs);
    let spawned = std::thread::Builder::new()
        .name("kirie-workshop".to_owned())
        .spawn(move || match request {
            W::Search(args) => {
                let body = match search(&query_from_args(&args)) {
                    Ok(items) => to_json(&items),
                    Err(err) => error(&err.to_string()),
                };
                let _ = reply.send(body);
            }
            W::State(id) => {
                let body = match state(&id) {
                    Ok(item) => to_json(std::slice::from_ref(&item)),
                    Err(err) => error(&err.to_string()),
                };
                let _ = reply.send(body);
            }
            W::Job(number) => {
                let body = jobs
                    .get(number)
                    .map_or_else(|| error("no such job"), |job| job.to_json(number));
                let _ = reply.send(body);
            }
            W::Subscribe(id) => {
                let number = jobs.start(&id);
                // Answer before the download, not after it: this is the whole
                // reason subscriptions are jobs.
                match subscribe(&id) {
                    Ok(item) => {
                        let _ = reply.send(serde_json::json!({ "job": number, "id": item.id }).to_string());
                        follow_download(&jobs, number, &item);
                    }
                    Err(err) => {
                        let message = err.to_string();
                        jobs.update(number, |job| {
                            job.state = "error";
                            job.error = Some(message.clone());
                        });
                        let _ = reply.send(error(&message));
                    }
                }
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "could not spawn a Workshop worker");
    }
}

/// Follow a subscription's download to its end, recording progress on the job.
///
/// Filesystem-only, like [`wait_for_install`]: asking Steam would announce the
/// daemon as playing Wallpaper Engine on every poll.
fn follow_download(jobs: &Jobs, number: u64, item: &Item) {
    if let Some(dir) = &item.dir {
        jobs.update(number, |job| {
            job.state = "installed";
            job.dir = Some(dir.clone());
        });
        return;
    }

    jobs.update(number, |job| job.state = "downloading");
    match wait_for_install(&item.id, WAIT_TIMEOUT, |bytes| {
        jobs.update(number, |job| job.bytes = bytes);
    }) {
        Ok(dir) => jobs.update(number, |job| {
            job.state = "installed";
            job.dir = Some(dir);
        }),
        Err(err) => jobs.update(number, |job| {
            job.state = "error";
            job.error = Some(err.to_string());
        }),
    }
}

/// Run `kirie workshop search`.
///
/// # Errors
/// When the query itself could not be run — see [`search`].
pub fn run_search(query: &Query, json: bool) -> Result<()> {
    let items = search(query)?;

    if json {
        println!("{}", to_json(&items));
        return Ok(());
    }

    if items.is_empty() {
        println!("no Workshop items matched");
        return Ok(());
    }

    let width = items.iter().map(|i| i.id.len()).max().unwrap_or(0);
    for item in &items {
        // Mirrors `kirie list`: a blank column is a wallpaper this build can
        // render, `!` one it cannot, `-` an asset that was never meant to be.
        let mark = match (item.renderable, item.kind) {
            (true, _) => ' ',
            (false, "asset") => '-',
            (false, _) => '!',
        };
        let here = if item.installed {
            "installed"
        } else if item.subscribed {
            "subscribed"
        } else {
            ""
        };
        println!(
            "{mark} {:width$}  {:<11} {:>3}%  {:>6}  {:<10} {}",
            item.id,
            item.kind,
            (item.score * 100.0).round() as i32,
            human_size(item.size),
            here,
            item.title,
            width = width
        );
    }

    println!(
        "\n{} result(s). Subscribe with: kirie workshop subscribe <id>",
        items.len()
    );
    Ok(())
}

/// How long `--wait` gives Steam before handing the user back their shell.
///
/// Generous on purpose: a large scene wallpaper on a slow line takes minutes,
/// and giving up does not cancel anything — Steam keeps downloading.
const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Run `kirie workshop subscribe`.
///
/// `wait` blocks until the files land; `apply` implies it, then shows the
/// wallpaper on that screen through a running engine's control socket.
///
/// # Errors
/// When Steam refuses the subscription, the download does not finish within
/// [`WAIT_TIMEOUT`], or no engine is listening for `--apply`.
pub fn run_subscribe(id: &str, wait: bool, apply_to: Option<&str>, socket: &Path, json: bool) -> Result<()> {
    let item = subscribe(id)?;

    // JSON is a machine's view of one moment; waiting is a human affordance,
    // so the two do not mix — a caller that wants progress watches the
    // directory itself, exactly as this does.
    if json {
        println!("{}", to_json(std::slice::from_ref(&item)));
        return Ok(());
    }

    println!("subscribed to {} ({})", item.id, item.title);
    if let Some(reason) = &item.reason {
        println!("note: {reason}");
    }

    let dir = match &item.dir {
        Some(dir) => {
            println!("already installed at {}", dir.display());
            dir.clone()
        }
        None if wait || apply_to.is_some() => {
            print!("downloading… ");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let dir = wait_for_install(id, WAIT_TIMEOUT, |bytes| {
                // One line, rewritten: a progress log that scrolls is noise in
                // a terminal and garbage in a pipe.
                print!("\rdownloading… {}   ", human_size(bytes));
                let _ = std::io::Write::flush(&mut std::io::stdout());
            })?;
            println!("\rinstalled at {}   ", dir.display());
            dir
        }
        None => {
            println!("Steam is downloading it; `kirie list` will show it once it lands");
            return Ok(());
        }
    };

    if let Some(screen) = apply_to {
        apply(socket, screen, &dir)?;
        println!("applied to {screen}");
    }
    Ok(())
}

/// Run `kirie workshop state`.
///
/// # Errors
/// As [`state`].
pub fn run_state(id: &str, json: bool) -> Result<()> {
    let item = state(id)?;
    if json {
        println!("{}", to_json(std::slice::from_ref(&item)));
        return Ok(());
    }

    println!("{} {}", item.id, item.title);
    println!("  type:       {}", item.kind);
    println!("  subscribed: {}", item.subscribed);
    println!("  installed:  {}", item.installed);
    if let Some(dir) = &item.dir {
        println!("  directory:  {}", dir.display());
    }
    if let Some(reason) = &item.reason {
        println!("  cannot render: {reason}");
    }
    Ok(())
}

/// Bytes as a short human string (Steam reports sizes for items nobody has
/// downloaded, so this is the only place they are formatted).
fn human_size(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display size, rounded to one decimal"
    )]
    let value = bytes as f64;
    for (unit, scale) in [
        ("G", 1024.0 * 1024.0 * 1024.0),
        ("M", 1024.0 * 1024.0),
        ("K", 1024.0),
    ] {
        if value >= scale {
            return format!("{:.1}{unit}", value / scale);
        }
    }
    format!("{bytes}B")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_search_result() {
        let value = serde_json::json!({
            "id": "1234",
            "title": "Test wallpaper",
            "tags": ["Scene", "Anime", "Approved"],
            "preview_url": "https://example.invalid/preview.jpg",
            "size": 4_275_510u64,
            "votes_up": 100,
            "votes_down": 2,
            "score": 0.97,
            "updated": 1_526_612_969u64,
        });
        let item = item_from(&value).expect("a result with an id parses");
        assert_eq!(item.kind, "scene");
        assert!(item.renderable);
        assert!(!item.installed);
        assert_eq!(item.dir, None);
        assert_eq!(item.votes, (100, 2));
    }

    #[test]
    fn an_application_item_is_reported_unrenderable() {
        let value = serde_json::json!({
            "id": "1",
            "tags": ["Wallpaper", "Application"],
        });
        let item = item_from(&value).expect("parses");
        assert_eq!(item.kind, "application");
        assert!(!item.renderable);
        assert!(item.reason.is_some());
    }

    #[test]
    fn hostile_input_never_panics() {
        // V9: the helper's output is untrusted. Nothing here may abort.
        for value in [
            serde_json::json!({}),
            serde_json::json!({ "id": 5 }),
            serde_json::json!({ "id": "1", "tags": "not-an-array" }),
            serde_json::json!({ "id": "1", "score": "high", "votes_up": -3 }),
            serde_json::json!({ "id": "1", "size": u64::MAX }),
            serde_json::json!({ "id": "1", "updated": u64::MAX }),
        ] {
            let _ = item_from(&value);
        }
    }

    #[test]
    fn a_result_without_an_id_is_dropped_not_guessed() {
        assert!(item_from(&serde_json::json!({ "title": "nameless" })).is_err());
    }

    #[test]
    fn tag_count_is_bounded() {
        let tags: Vec<String> = (0..500).map(|n| format!("tag{n}")).collect();
        let value = serde_json::json!({ "id": "1", "tags": tags });
        let item = item_from(&value).expect("parses");
        assert!(item.tags.len() <= 64);
    }

    #[test]
    fn sizes_read_as_they_would_be_shown() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(4_275_510), "4.1M");
    }
}
