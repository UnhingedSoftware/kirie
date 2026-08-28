#[cfg(feature = "tui")]
pub mod tui;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::compat::resolve;

const PAGE_SIZE: usize = 50;

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub dir: Option<PathBuf>,
    pub preview: Option<String>,
    pub renderable: bool,
    pub reason: Option<String>,
    pub subscribed: bool,
    pub installed: bool,
    pub size: u64,
    pub votes: (u32, u32),
    pub score: f32,
    pub updated: u32,
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Query {
    pub text: Option<String>,
    pub tags: Vec<String>,
    pub excluded_tags: Vec<String>,
    pub match_any_tag: bool,
    pub sort: String,
    pub trend_days: Option<u32>,
    pub page: u32,
    pub limit: Option<usize>,
}

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

pub fn subscribe(id: &str) -> Result<Item> {
    let answer = ask_helper(&["subscribe".to_owned(), id.to_owned()])?;
    let mut item = item_from(&answer)?;
    mark_local(std::slice::from_mut(&mut item));
    Ok(item)
}

pub fn unsubscribe(id: &str) -> Result<Item> {
    let answer = ask_helper(&["unsubscribe".to_owned(), id.to_owned()])?;
    let mut item = item_from(&answer)?;
    mark_local(std::slice::from_mut(&mut item));
    Ok(item)
}

pub fn state(id: &str) -> Result<Item> {
    let answer = ask_helper(&["state".to_owned(), id.to_owned()])?;
    let mut item = item_from(&answer)?;
    mark_local(std::slice::from_mut(&mut item));
    Ok(item)
}

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

fn ask_helper(args: &[String]) -> Result<serde_json::Value> {
    let (helper, prefix) = helper_command()?;

    let mut command = std::process::Command::new(&helper);
    command.args(prefix);
    command.args(args);
    for root in crate::compat::steam::libraries() {
        command.arg(root);
    }
    command.stderr(std::process::Stdio::null());

    let output = command
        .output()
        .with_context(|| format!("could not run {}", helper.display()))?;
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

pub fn probe() -> Result<bool> {
    let answer = ask_helper(&["probe".to_owned()])?;
    if let Some(detail) = answer.get("detail").and_then(serde_json::Value::as_str) {
        return Err(anyhow!(detail.to_owned()));
    }
    Ok(answer
        .get("owned")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

fn helper_command() -> Result<(PathBuf, Vec<String>)> {
    if let Some(explicit) = std::env::var_os("KIRIE_STEAM_HELPER") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok((path, Vec::new()));
        }
    }
    let exe = std::env::current_exe().context("could not find this binary")?;
    Ok((exe, vec![kirie_steam_helper::HELPER_ARG.to_owned()]))
}

#[must_use]
pub fn helper_path() -> Option<PathBuf> {
    helper_command().ok().map(|(program, _)| program)
}

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

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub state: &'static str,
    pub bytes: u64,
    pub dir: Option<PathBuf>,
    pub error: Option<String>,
}

impl Job {
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

#[derive(Debug, Default)]
pub struct Jobs {
    inner: std::sync::Mutex<JobTable>,
}

#[derive(Debug, Default)]
struct JobTable {
    next: u64,
    jobs: std::collections::HashMap<u64, Job>,
}

impl Jobs {
    fn start(&self, id: &str) -> u64 {
        let Ok(mut table) = self.inner.lock() else {
            return 0;
        };
        table.next += 1;
        let number = table.next;
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

    fn update(&self, number: u64, edit: impl FnOnce(&mut Job)) {
        if let Ok(mut table) = self.inner.lock()
            && let Some(job) = table.jobs.get_mut(&number)
        {
            edit(job);
        }
    }

    fn get(&self, number: u64) -> Option<Job> {
        self.inner.lock().ok()?.jobs.get(&number).cloned()
    }
}

#[must_use]
pub fn query_from_args(line: &str) -> Query {
    let mut query = Query {
        sort: "popular".to_owned(),
        page: 1,
        ..Query::default()
    };

    let mut rest = line.trim();
    while !rest.is_empty() {
        let (token, tail) = split_token(rest);
        let Some((key, value)) = token.split_once('=') else {
            rest = tail;
            continue;
        };
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        match key {
            "text" => {
                let quoted = token.starts_with("text=\"");
                let phrase = if quoted || tail.is_empty() {
                    value.to_owned()
                } else {
                    format!("{value} {tail}")
                };
                query.text = Some(phrase.trim().to_owned());
                if !quoted {
                    return query;
                }
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

fn split_token(rest: &str) -> (&str, &str) {
    let quote_at = rest.find("=\"");
    let end = match quote_at {
        Some(open) => rest[open + 2..]
            .find('"')
            .map_or(rest.len(), |close| open + 2 + close + 1),
        None => rest.len(),
    };
    let space = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let cut = if quote_at.is_some_and(|at| at < space) {
        end
    } else {
        space
    };
    (&rest[..cut], rest[cut..].trim_start())
}

pub fn serve_socket(
    jobs: &std::sync::Arc<Jobs>,
    request: kirie_ipc::WorkshopRequest,
    reply: crossbeam_channel::Sender<String>,
) {
    use kirie_ipc::WorkshopRequest as W;

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
            W::Unsubscribe(id) => {
                let body = match unsubscribe(&id) {
                    Ok(item) => to_json(std::slice::from_ref(&item)),
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

const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub fn run_subscribe(id: &str, wait: bool, apply_to: Option<&str>, socket: &Path, json: bool) -> Result<()> {
    let item = subscribe(id)?;

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

pub fn run_unsubscribe(id: &str, json: bool) -> Result<()> {
    let item = unsubscribe(id)?;
    if json {
        println!("{}", to_json(std::slice::from_ref(&item)));
        return Ok(());
    }

    println!("unsubscribed from {} ({})", item.id, item.title);
    if item.installed {
        println!("Steam removes the files when it next runs");
    }
    Ok(())
}

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
    fn a_quoted_tag_keeps_its_spaces() {
        let query = query_from_args(r#"tag="Audio responsive" tag=Scene sort=trend"#);
        assert_eq!(query.tags, vec!["Audio responsive", "Scene"]);
        assert_eq!(query.sort, "trend");
    }

    #[test]
    fn a_quoted_phrase_lets_filters_follow_it() {
        let query = query_from_args(r#"text="blue sky" tag=Scene"#);
        assert_eq!(query.text.as_deref(), Some("blue sky"));
        assert_eq!(query.tags, vec!["Scene"]);
    }

    #[test]
    fn a_bare_phrase_still_takes_the_rest_of_the_line() {
        let query = query_from_args("sort=recent text=blue sky at night");
        assert_eq!(query.text.as_deref(), Some("blue sky at night"));
        assert_eq!(query.sort, "recent");
    }

    #[test]
    fn an_unterminated_quote_does_not_eat_the_query() {
        let query = query_from_args(r#"tag="Puppet Warp sort=trend"#);
        assert_eq!(query.tags, vec![r#""Puppet Warp sort=trend"#]);
    }

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
