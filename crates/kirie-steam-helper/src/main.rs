//! Standalone `kirie-steam-helper`.
//!
//! kirie carries this same code and re-execs itself to run it, so nothing
//! installs this binary. It stays as a way to exercise a verb by hand:
//! `kirie-steam-helper probe ~/.local/share/Steam`.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    kirie_steam_helper::run(&args)
}
