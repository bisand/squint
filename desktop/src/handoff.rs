//! Files to open that did not come from squint's own Open dialog: the ones a
//! second squint was started with, and the ones the system hands a running
//! one.
//!
//! squint is one editor window, so a file double-clicked in a file manager
//! while it runs belongs in a tab of that window, not in a second squint with
//! its own tabs and its own idea of the session. The first squint listens on a
//! local socket — a Unix socket in a directory only this user can use, or a
//! named pipe on Windows — and a squint started with files while one listens
//! sends them there and exits. A squint started with no files is somebody
//! asking for a squint, and gets one.
//!
//! Whatever arrives waits in a queue the window takes from between frames, as
//! do the files macOS hands over by Apple Event (see `open_documents`).

use denise_winit::Waker;
use interprocess::local_socket::{ListenerOptions, Name, Stream, prelude::*};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;

/// Files waiting for the window, oldest first.
static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Signalled whenever files join the queue. The window does not wait on it —
/// it is woken by `WAKER` and takes what is there between frames — but
/// anybody who must wait for an arrival can do so on this rather than by
/// asking the queue again and again against a clock.
static ARRIVED: Condvar = Condvar::new();

/// Wakes the window when files arrive, once there is a window to wake.
static WAKER: OnceLock<Waker> = OnceLock::new();

/// What a message starts with, so a stray connection is not read as files.
const MAGIC: &[u8] = b"squint-open-1\0";

/// More than any list of files anybody drops on an icon.
const MOST: u64 = 1 << 20;

/// Adds files for the window to open, and wakes it to open them.
pub fn push(paths: impl IntoIterator<Item = PathBuf>) {
    if let Ok(mut opened) = OPENED.lock() {
        opened.extend(paths);
    }
    ARRIVED.notify_all();
    if let Some(waker) = WAKER.get() {
        waker.wake();
    }
}

/// How to wake the window when files arrive. Files that came before it are
/// taken up by the window's first frame.
pub fn wake_with(waker: Waker) {
    let _ = WAKER.set(waker);
}

/// The files waiting to be opened, oldest first.
pub fn take() -> Vec<PathBuf> {
    OPENED
        .lock()
        .map(|mut opened| std::mem::take(&mut *opened))
        .unwrap_or_default()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// This squint runs: it is the first, or could not reach the first.
    Run,
    /// The squint already running took `paths`, and this one has nothing to do.
    HandedOver,
}

/// Hands `paths` to a squint already running, or becomes the squint that
/// others hand theirs to.
pub fn claim(paths: &[PathBuf]) -> Claim {
    match address() {
        Some(address) => claim_at(&address, paths),
        None => Claim::Run,
    }
}

fn claim_at(address: &Address, paths: &[PathBuf]) -> Claim {
    if let Some(name) = address.name()
        && let Ok(stream) = Stream::connect(name)
    {
        // One is running. With no files this squint is a second window, as
        // asked, and leaves the socket to the first.
        if paths.is_empty() {
            return Claim::Run;
        }
        if send(stream, paths).is_ok() {
            return Claim::HandedOver;
        }
        return Claim::Run;
    }
    // Nobody answers, so a socket file left there is from a squint that has
    // gone, and can be replaced.
    let Some(name) = address.name() else {
        return Claim::Run;
    };
    let Ok(listener) = ListenerOptions::new()
        .name(name)
        .try_overwrite(true)
        .create_sync()
    else {
        // squint still runs; it only cannot be handed files.
        return Claim::Run;
    };
    address.restrict();
    thread::Builder::new()
        .name("handoff".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Ok(paths) = receive(stream) {
                    push(paths);
                }
            }
        })
        .ok();
    Claim::Run
}

fn send(mut stream: Stream, paths: &[PathBuf]) -> io::Result<()> {
    let mut message = MAGIC.to_vec();
    for path in paths {
        message.extend_from_slice(&encode(path));
        message.push(0);
    }
    stream.write_all(&message)?;
    stream.flush()
}

fn receive(stream: Stream) -> io::Result<Vec<PathBuf>> {
    let mut message = Vec::new();
    stream.take(MOST).read_to_end(&mut message)?;
    let Some(rest) = message.strip_prefix(MAGIC) else {
        return Err(io::ErrorKind::InvalidData.into());
    };
    Ok(rest
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .filter_map(decode)
        .collect())
}

#[cfg(unix)]
fn encode(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

// A path Windows can name but UTF-8 cannot is not handed over; it is also
// not a path a file manager hands anybody.
#[cfg(not(unix))]
fn encode(path: &Path) -> Vec<u8> {
    path.to_str().unwrap_or_default().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn decode(bytes: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(bytes).ok().map(PathBuf::from)
}

/// Where the squint that takes files listens.
enum Address {
    /// A socket file, in a directory that is this user's.
    #[cfg_attr(not(unix), allow(dead_code))]
    File(PathBuf),
    /// A named pipe, named for this user.
    #[cfg_attr(unix, allow(dead_code))]
    Pipe(String),
}

impl Address {
    fn name(&self) -> Option<Name<'_>> {
        use interprocess::local_socket::{GenericFilePath, GenericNamespaced};
        match self {
            Address::File(path) => path.as_path().to_fs_name::<GenericFilePath>().ok(),
            Address::Pipe(name) => name.as_str().to_ns_name::<GenericNamespaced>().ok(),
        }
    }

    /// Makes the socket this user's alone to connect to.
    fn restrict(&self) {
        #[cfg(unix)]
        if let Address::File(path) = self {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(unix)]
fn address() -> Option<Address> {
    // The runtime directory is the user's own by definition; where there is
    // none — macOS — the configuration directory is.
    let dir = dirs::runtime_dir()
        .or_else(dirs::config_dir)?
        .join("squint");
    std::fs::create_dir_all(&dir).ok()?;
    Some(Address::File(dir.join("open.sock")))
}

#[cfg(not(unix))]
fn address() -> Option<Address> {
    let user = std::env::var("USERNAME").unwrap_or_default();
    Some(Address::Pipe(format!("squint-open-{user}")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_second_squint_hands_its_files_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let address = Address::File(dir.path().join("open.sock"));
        let files = [PathBuf::from("/tmp/a b.log"), PathBuf::from("/tmp/ü.json")];

        assert_eq!(claim_at(&address, &[]), Claim::Run, "the first runs");
        let mode = std::fs::metadata(dir.path().join("open.sock")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);

        assert_eq!(
            claim_at(&address, &[]),
            Claim::Run,
            "one with no files runs too"
        );
        assert_eq!(claim_at(&address, &files), Claim::HandedOver);

        // The listener took the files on its own thread, so wait to be told
        // it has rather than asking the queue until a clock runs out: on a
        // machine busy enough, that thread may not be scheduled before any
        // deadline worth writing down. The timeout here is only so a thread
        // that never arrives fails the test instead of hanging it.
        let mut opened = OPENED.lock().unwrap();
        while opened.len() < files.len() {
            let (next, timed_out) = ARRIVED
                .wait_timeout(opened, Duration::from_secs(60))
                .unwrap();
            assert!(!timed_out.timed_out(), "the listener never took the files");
            opened = next;
        }
        assert_eq!(std::mem::take(&mut *opened), files);
    }

    #[test]
    fn a_stray_message_is_not_read_as_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stray.sock");
        let name = path
            .as_path()
            .to_fs_name::<interprocess::local_socket::GenericFilePath>();
        let listener = ListenerOptions::new()
            .name(name.unwrap())
            .create_sync()
            .unwrap();
        let writer = thread::spawn(move || {
            let name = path
                .as_path()
                .to_fs_name::<interprocess::local_socket::GenericFilePath>();
            let mut stream = Stream::connect(name.unwrap()).unwrap();
            stream.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        });
        let stream = listener.incoming().next().unwrap().unwrap();
        writer.join().unwrap();
        assert!(receive(stream).is_err());
    }
}
