use std::process::ExitCode;

fn main() -> ExitCode {
    kirie::run(std::env::args_os().collect())
}
