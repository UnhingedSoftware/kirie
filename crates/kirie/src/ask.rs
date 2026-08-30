use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(20);

pub fn run(socket: &Path, line: &str) -> Result<String, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|err| format!("no renderer answering on {} ({err})", socket.display()))?;
    stream.set_read_timeout(Some(DEADLINE)).ok();
    stream.set_write_timeout(Some(DEADLINE)).ok();

    let mut writing = &stream;
    writeln!(writing, "{line}").map_err(|err| format!("could not ask ({err})"))?;
    writing.flush().ok();

    let mut said = String::new();
    for read in BufReader::new(&stream).lines() {
        let read = read.map_err(|err| format!("could not read the answer ({err})"))?;
        said.push_str(&read);
        said.push('\n');
    }
    Ok(said)
}
