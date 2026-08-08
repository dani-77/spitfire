//! spitfire's control socket — a Unix socket at [`default_socket_path`],
//! newline-delimited JSON, one [`Request`]/[`Response`] pair per
//! connection. Modeled after `niri msg`/`hyprctl`: `spitfirectl` (see
//! `src/bin/spitfirectl.rs`) is a thin client over this.
//!
//! This crate only knows about the wire protocol and socket plumbing — it
//! has no Smithay/Wayland dependency. Turning a [`Request`] into an actual
//! effect on the compositor happens in the `spitfire` crate
//! (`crates/spitfire/src/ipc.rs`), which is the one with access to
//! `SpitfireState`.

mod protocol;

pub use protocol::{Request, Response};

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
};

/// `$XDG_RUNTIME_DIR/spitfire.sock`, falling back to
/// `/tmp/spitfire-$USER.sock` if `XDG_RUNTIME_DIR` isn't set.
pub fn default_socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("spitfire.sock");
        }
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    std::env::temp_dir().join(format!("spitfire-{user}.sock"))
}

/// Writes `msg` as one JSON line (used for both requests and responses —
/// the framing is identical on both sides of the connection).
pub fn write_message<T: serde::Serialize>(stream: &mut impl Write, msg: &T) -> io::Result<()> {
    let json = serde_json::to_string(msg)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Reads one JSON line. Returns `UnexpectedEof` if the peer closed the
/// connection without sending anything.
pub fn read_message<T: serde::de::DeserializeOwned>(stream: &mut impl BufRead) -> io::Result<T> {
    let mut line = String::new();
    let n = stream.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Client side: connects to `path`, sends `request`, and waits for the one
/// response line. Used by `spitfirectl`.
pub fn send_request(path: &Path, request: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(path)?;
    write_message(&mut stream, request)?;
    let mut reader = BufReader::new(stream);
    read_message(&mut reader)
}

/// Server side: binds the control socket at `path`, in non-blocking mode
/// so it can be driven by an event loop (see `crates/spitfire/src/ipc.rs`).
///
/// If a socket file is already there, it's removed first — assumed to be
/// left over from a previous run that didn't shut down cleanly. This means
/// a second spitfire instance silently steals the socket from a still-running
/// first one instead of failing loudly; detecting "already running" is a
/// known v1 limitation.
pub fn bind_listener(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn message_round_trips_through_the_line_framing() {
        let req = Request::SetLayout {
            mode: "monocle".into(),
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &req).unwrap();
        assert_eq!(buf.last(), Some(&b'\n'));

        let mut reader = Cursor::new(buf);
        let read_back: Request = read_message(&mut reader).unwrap();
        assert_eq!(read_back, req);
    }

    #[test]
    fn reading_from_a_closed_stream_is_unexpected_eof() {
        let mut reader = Cursor::new(Vec::<u8>::new());
        let result: io::Result<Request> = read_message(&mut reader);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn default_socket_path_uses_xdg_runtime_dir_when_set() {
        // SAFETY: this test only runs in this process; no other thread here
        // reads/writes XDG_RUNTIME_DIR concurrently.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        assert_eq!(
            default_socket_path(),
            PathBuf::from("/run/user/1000/spitfire.sock")
        );
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    fn bind_listener_replaces_a_stale_socket_file() {
        let path = std::env::temp_dir().join(format!(
            "spitfire-ipc-test-{:?}.sock",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);

        let first = bind_listener(&path).unwrap();
        // Simulate a stale socket left behind by a crashed run: the file
        // exists on disk, but nothing is accepting on it anymore.
        drop(first);
        assert!(path.exists());

        let second = bind_listener(&path);
        assert!(
            second.is_ok(),
            "bind_listener should clean up a stale socket file"
        );

        let _ = std::fs::remove_file(&path);
    }
}
