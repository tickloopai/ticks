//! The local socket between the native host and the Ticks: a Unix domain socket that only the user
//! can reach, at a path both sides derive from the user alone. The native host serves it and each
//! session's Tick connects to it. Chrome started from the desktop and Tickloop started from a shell
//! see different environments, so no environment variable takes part.

use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
};

use tokio::net::{UnixListener, UnixStream};

/// The socket's name in its directory.
pub const SOCKET_NAME: &str = "tick.sock";
/// The file a serving host holds locked, beside the socket: whoever holds it owns the socket.
pub const LOCK_NAME: &str = "tick.lock";

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Where the socket lives unless a test names another directory: `tick-browser-<uid>` in the user's
/// own temporary directory.
pub fn default_directory() -> io::Result<PathBuf> {
    Ok(base()?.join(format!("tick-browser-{}", uid())))
}

/// The user's temporary directory as the system names it for every process of the user.
#[cfg(target_os = "macos")]
fn base() -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if length == 0 || length > buffer.len() {
        return Err(io::Error::other(
            "the system names no temporary directory for this user",
        ));
    }
    // The length counts the terminating NUL.
    buffer.truncate(length - 1);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

/// The user's runtime directory when the system made one for the user, and `/tmp` otherwise.
#[cfg(not(target_os = "macos"))]
fn base() -> io::Result<PathBuf> {
    let runtime = PathBuf::from(format!("/run/user/{}", uid()));
    Ok(match std::fs::symlink_metadata(&runtime) {
        Ok(metadata) if is_private(&metadata) => runtime,
        _ => PathBuf::from("/tmp"),
    })
}

/// A directory of the user's that no one else can enter.
fn is_private(metadata: &std::fs::Metadata) -> bool {
    metadata.is_dir() && metadata.uid() == uid() && metadata.mode() & 0o077 == 0
}

/// Checks that `directory` is the user's own, private directory, as a symbolic link is not.
fn check_private(directory: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(directory)?;
    if is_private(&metadata) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be a directory of this user's that others cannot enter",
                directory.display()
            ),
        ))
    }
}

/// A file's device and inode, which tell it apart from another one put at its path.
type Identity = (u64, u64);

fn identity(path: &Path) -> io::Result<Identity> {
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// The socket a native host serves while it holds the lock beside it. Only the socket this host bound
/// is removed when it stops serving, never one that took its place.
pub struct Listening {
    pub listener: UnixListener,
    path: PathBuf,
    socket: Identity,
    lock_path: PathBuf,
    lock: Identity,
    /// Held open, and so locked, until the host stops serving; the lock file itself stays.
    _lock: File,
}

impl Listening {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keeps the socket where the Ticks look for it. The system removes temporary files nobody has
    /// used for days, so the directory, the lock and the socket are touched; a socket that is gone
    /// or replaced anyway is bound again, while this host's lock is still the one in the directory.
    /// True when the socket was bound again.
    pub fn refresh(&mut self) -> io::Result<bool> {
        let directory = self.path.parent().expect("the socket has a directory");
        touch(directory)?;
        if identity(&self.lock_path).ok() != Some(self.lock) {
            return Err(io::Error::other(format!(
                "{} was removed while this host held it",
                self.lock_path.display()
            )));
        }
        touch(&self.lock_path)?;
        if identity(&self.path).ok() == Some(self.socket) {
            touch(&self.path)?;
            return Ok(false);
        }
        let (listener, socket) = bind(&self.path)?;
        self.listener = listener;
        self.socket = socket;
        Ok(true)
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        if identity(&self.path).ok() == Some(self.socket) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Sets `path`'s times to now, without following a symbolic link.
fn touch(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            path.as_ptr(),
            std::ptr::null(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Binds the socket at `path`, replacing what is there; only the holder of the lock does this.
fn bind(path: &Path) -> io::Result<(UnixListener, Identity)> {
    match std::fs::remove_file(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, identity(path)?))
}

/// Takes `file`'s lock with `operation` without waiting: false when another holder keeps it out.
fn lock(file: &File, operation: libc::c_int) -> io::Result<bool> {
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Serves the socket in `directory`, creating the directory private to the user. Refused with
/// `AddrInUse` while another host holds the lock there; a socket left by a host that died is
/// replaced.
pub fn listen(directory: &Path) -> io::Result<Listening> {
    match std::fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    check_private(directory)?;
    let lock_path = directory.join(LOCK_NAME);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)?;
    if !lock(&lock_file, libc::LOCK_EX)? {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "another browser is serving Tickloop; close it there to use this one",
        ));
    }
    let path = directory.join(SOCKET_NAME);
    let (listener, socket) = bind(&path)?;
    Ok(Listening {
        listener,
        path,
        socket,
        lock: identity(&lock_path)?,
        lock_path,
        _lock: lock_file,
    })
}

/// Whether a native host serves the socket in `directory`, told by its lock without connecting, so
/// the host sees nothing of the question.
pub fn is_served(directory: &Path) -> bool {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join(LOCK_NAME))
    else {
        return false;
    };
    // A shared lock taken and let go at once; it fails only while a host holds the lock.
    matches!(lock(&file, libc::LOCK_SH), Ok(false))
}

/// Connects to the native host serving the socket in `directory`, which must be the user's private
/// one.
pub async fn connect(directory: &Path) -> io::Result<UnixStream> {
    check_private(directory)?;
    UnixStream::connect(directory.join(SOCKET_NAME)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_socket_is_private_to_the_user_and_served_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("socket");
        let listening = listen(&directory).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o777;
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(listening.path()), 0o600);
        assert_eq!(listening.path(), directory.join(SOCKET_NAME));

        let error = listen(&directory).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        let connected = connect(&directory).await.unwrap();
        let (accepted, _) = listening.listener.accept().await.unwrap();
        drop((connected, accepted));

        let path = listening.path().to_owned();
        drop(listening);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_socket_left_by_a_host_that_died_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("socket");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        // A socket nothing serves any more, as a killed host leaves it.
        drop(std::os::unix::net::UnixListener::bind(directory.join(SOCKET_NAME)).unwrap());
        let listening = listen(&directory).unwrap();
        assert!(connect(&directory).await.is_ok());
        drop(listening);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn of_hosts_started_at_once_only_one_serves() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("socket");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let opened: Vec<_> = (0..8)
            .map(|_| {
                let (directory, barrier) = (directory.clone(), barrier.clone());
                tokio::task::spawn_blocking(move || {
                    barrier.wait();
                    listen(&directory)
                })
            })
            .collect();
        let mut serving = Vec::new();
        for opened in opened {
            match opened.await.unwrap() {
                Ok(listening) => serving.push(listening),
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::AddrInUse),
            }
        }
        assert_eq!(serving.len(), 1);
        assert!(is_served(&directory));
        let connected = connect(&directory).await.unwrap();
        let (accepted, _) = serving[0].listener.accept().await.unwrap();
        drop((connected, accepted, serving));
        assert!(!is_served(&directory));
    }

    #[tokio::test]
    async fn a_socket_removed_under_a_host_is_bound_again_and_never_lets_a_second_host_in() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("socket");
        let mut first = listen(&directory).unwrap();
        std::fs::remove_file(first.path()).unwrap();
        // Another host started now still finds the lock held.
        assert_eq!(
            listen(&directory).err().unwrap().kind(),
            io::ErrorKind::AddrInUse
        );
        assert!(connect(&directory).await.is_err());
        assert!(first.refresh().unwrap());
        let connected = connect(&directory).await.unwrap();
        drop((connected, first.listener.accept().await.unwrap()));
        assert!(!first.refresh().unwrap());

        // Whatever took the socket's place is not removed when the host stops serving.
        std::fs::remove_file(first.path()).unwrap();
        let other = std::os::unix::net::UnixListener::bind(first.path()).unwrap();
        let path = first.path().to_owned();
        drop(first);
        assert!(path.exists());
        drop(other);
    }

    #[tokio::test]
    async fn a_directory_others_can_enter_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("shared");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&directory)
            .unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            listen(&directory).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            connect(&directory).await.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let link = root.path().join("link");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        assert!(listen(&link).is_err());
    }

    #[test]
    fn the_default_directory_belongs_to_this_user() {
        let directory = default_directory().unwrap();
        assert!(directory.is_absolute());
        assert_eq!(
            directory.file_name().unwrap().to_str().unwrap(),
            format!("tick-browser-{}", uid())
        );
        // A Unix socket path has room for about a hundred bytes.
        assert!(directory.join(SOCKET_NAME).as_os_str().len() < 100);
    }
}
