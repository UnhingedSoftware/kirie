//! Ignored release-gate soak: proves the live build→render→drop cycle (every
//! `bg` swap) is leak-free — bounded RSS across multiple full corpus cycles and
//! stable open-fd count. Needs a GPU + the installed workshop corpus, so it is
//! `#[ignore]`:
//!
//! ```text
//! cargo test -p kirie --release --test soak -- --ignored --nocapture
//! ```
//!
//! Point it at any corpus of workshop item folders with `KIRIE_SOAK_DIR`.

use std::path::PathBuf;

fn corpus_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("KIRIE_SOAK_DIR") {
        let p = PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join(".local/share/Steam/steamapps/workshop/content/431960");
    p.is_dir().then_some(p)
}

#[test]
#[ignore = "needs a GPU and the installed workshop corpus"]
fn soak_is_leak_free() {
    let Some(dir) = corpus_dir() else {
        eprintln!("soak test skipped: no corpus (set KIRIE_SOAK_DIR to a workshop content dir)");
        return;
    };

    // ~4 full cycles of a ~20-item corpus + margin — enough to separate a real
    // plateau from a slow leak while staying quick.
    let report = kirie::soak::soak(&dir, 80, 4, 20).expect("soak run");

    assert_eq!(report.fails, 0, "some wallpapers failed to build: {report:?}");
    assert!(
        report.fd_end <= report.fd_start + 8,
        "fd leak: {} -> {} (peak {})",
        report.fd_start,
        report.fd_end,
        report.fd_peak
    );
    // With per-iteration `trim_heap` the end RSS returns to the warm baseline
    // (post first full cycle); a real leak of tens of MB/iter would blow far
    // past this generous bound.
    let cap = report.rss_warm_kb * 3 / 2 + 262_144;
    assert!(
        report.rss_end_kb <= cap,
        "RSS leak: warm={}KB end={}KB peak={}KB cap={}KB",
        report.rss_warm_kb,
        report.rss_end_kb,
        report.rss_peak_kb,
        cap
    );
}
