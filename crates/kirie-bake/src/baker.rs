use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use notify::{RecursiveMode, Watcher};

use crate::bundle::BundleContent;
use crate::cache::Cache;
use crate::error::BakeError;
use crate::gc;

pub type SourceFn = Arc<dyn Fn(&Path) -> Result<Vec<u8>, BakeError> + Send + Sync>;

pub type ContentFn = Arc<dyn Fn(&Path, &[u8]) -> Result<BundleContent, BakeError> + Send + Sync>;

pub type PauseFn = Arc<dyn Fn() -> bool + Send + Sync>;

#[must_use]
pub fn never_pause() -> PauseFn {
    Arc::new(|| false)
}

#[derive(Clone)]
pub struct BakerConfig {
    pub cache: Cache,
    pub source_fn: SourceFn,
    pub content_fn: ContentFn,
    pub should_pause: PauseFn,
    pub cap_bytes: u64,
    pub num_threads: usize,
}

impl BakerConfig {
    #[must_use]
    pub fn new(cache: Cache, source_fn: SourceFn, content_fn: ContentFn) -> Self {
        BakerConfig {
            cache,
            source_fn,
            content_fn,
            should_pause: never_pause(),
            cap_bytes: gc::DEFAULT_CAP_BYTES,
            num_threads: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BakeOutcome {
    Baked(PathBuf),
    Fresh,
    Paused,
}

struct Inner {
    cache: Cache,
    source_fn: SourceFn,
    content_fn: ContentFn,
    should_pause: PauseFn,
    paused: AtomicBool,
    shutdown: AtomicBool,
    cap_bytes: u64,
}

impl Inner {
    fn paused_now(&self) -> bool {
        self.paused.load(Ordering::Relaxed) || (self.should_pause)()
    }

    fn bake_item(&self, item: &Path) -> Result<BakeOutcome, BakeError> {
        if self.paused_now() {
            return Ok(BakeOutcome::Paused);
        }
        let source = (self.source_fn)(item)?;
        if self.cache.load(&source)?.is_some() {
            return Ok(BakeOutcome::Fresh);
        }
        if self.paused_now() {
            return Ok(BakeOutcome::Paused);
        }
        let content = (self.content_fn)(item, &source)?;
        let path = self.cache.bake(&source, content)?;
        if let Err(e) = gc::gc(&self.cache.bundles_dir(), self.cap_bytes) {
            tracing::warn!(error = %e, "post-bake gc failed");
        }
        Ok(BakeOutcome::Baked(path))
    }
}

enum Msg {
    Item(PathBuf),
    Stop,
}

pub struct BackgroundBaker {
    inner: Arc<Inner>,
    tx: Sender<Msg>,
    coordinator: Option<JoinHandle<()>>,
    watchers: Vec<notify::RecommendedWatcher>,
}

impl BackgroundBaker {
    #[must_use]
    pub fn start(config: BakerConfig) -> Self {
        let inner = Arc::new(Inner {
            cache: config.cache,
            source_fn: config.source_fn,
            content_fn: config.content_fn,
            should_pause: config.should_pause,
            paused: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            cap_bytes: config.cap_bytes,
        });
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(config.num_threads.max(1))
            .thread_name(|i| format!("kirie-bake-{i}"))
            .build()
            .expect("rayon pool");
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = crossbeam_channel::unbounded();
        let coord_inner = Arc::clone(&inner);
        let coordinator = std::thread::Builder::new()
            .name("kirie-bake-coord".into())
            .spawn(move || coordinator_loop(&coord_inner, &pool, &rx))
            .expect("spawn coordinator");
        BackgroundBaker {
            inner,
            tx,
            coordinator: Some(coordinator),
            watchers: Vec::new(),
        }
    }

    pub fn enqueue(&self, item: impl Into<PathBuf>) {
        let _ = self.tx.send(Msg::Item(item.into()));
    }

    pub fn watch(&mut self, dir: impl Into<PathBuf>) -> Result<(), BakeError> {
        let dir = dir.into();
        let tx = self.tx.clone();
        let root = dir.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if !matches!(
                event.kind,
                notify::EventKind::Create(_) | notify::EventKind::Modify(_)
            ) {
                return;
            }
            for path in event.paths {
                if let Some(item) = item_root(&root, &path) {
                    let _ = tx.send(Msg::Item(item));
                }
            }
        })
        .map_err(|e| BakeError::Watch(e.to_string()))?;
        watcher
            .watch(&dir, RecursiveMode::Recursive)
            .map_err(|e| BakeError::Watch(e.to_string()))?;
        self.watchers.push(watcher);
        Ok(())
    }

    pub fn bake_item_now(&self, item: &Path) -> Result<BakeOutcome, BakeError> {
        self.inner.bake_item(item)
    }

    pub fn pause(&self) {
        self.inner.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.inner.paused.store(false, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.inner.paused_now()
    }

    pub fn shutdown(&mut self) {
        self.watchers.clear();
        self.inner
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.tx.send(Msg::Stop);
        if let Some(h) = self.coordinator.take() {
            let _ = h.join();
        }
    }
}

impl Drop for BackgroundBaker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn coordinator_loop(inner: &Arc<Inner>, pool: &rayon::ThreadPool, rx: &Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        let item = match msg {
            Msg::Item(p) => p,
            Msg::Stop => break,
        };
        let job = Arc::clone(inner);
        pool.spawn(move || {
            loop {
                match job.bake_item(&item) {
                    Ok(BakeOutcome::Paused) => {
                        if job.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                    Ok(_) => return,
                    Err(e) => {
                        tracing::warn!(item = %item.display(), error = %e, "background bake failed");
                        return;
                    }
                }
            }
        });
    }
}

fn item_root(root: &Path, changed: &Path) -> Option<PathBuf> {
    let rel = changed.strip_prefix(root).ok()?;
    let first = rel.components().next()?;
    Some(root.join(first.as_os_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_root_maps_nested_change_to_child() {
        let root = Path::new("/ws");
        assert_eq!(
            item_root(root, Path::new("/ws/123/scene.pkg")),
            Some(PathBuf::from("/ws/123"))
        );
        assert_eq!(
            item_root(root, Path::new("/ws/123")),
            Some(PathBuf::from("/ws/123"))
        );
        assert_eq!(item_root(root, Path::new("/other/x")), None);
    }
}
