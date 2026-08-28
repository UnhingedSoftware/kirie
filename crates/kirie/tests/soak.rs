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

    let report = kirie::soak::soak(&dir, 80, 4, 20).expect("soak run");

    assert_eq!(report.fails, 0, "some wallpapers failed to build: {report:?}");
    assert!(
        report.fd_end <= report.fd_start + 8,
        "fd leak: {} -> {} (peak {})",
        report.fd_start,
        report.fd_end,
        report.fd_peak
    );
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
