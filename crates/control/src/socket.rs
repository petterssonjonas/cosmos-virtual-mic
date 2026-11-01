// SPDX-License-Identifier: MPL-2.0
//! Private Unix socket ownership. The persistent lock inode is never removed.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Component, Path, PathBuf};

use crate::invalid;

fn user_id() -> u32 {
    // SAFETY: geteuid has no pointer or lifetime requirements.
    unsafe { libc::geteuid() }
}

fn private(metadata: &Metadata) -> bool {
    metadata.uid() == user_id() && metadata.mode() & 0o077 == 0
}

/// Require an absolute, symlink-free path and a private, user-owned parent.
pub fn validate_parent(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(invalid("control socket must be an absolute file path"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("control socket has no parent"))?;
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component),
            _ => return Err(invalid("control socket path must not contain . or ..")),
        }
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() {
            return Err(invalid(
                "control socket parent must not contain symlinks or non-directories",
            ));
        }
        // A writable non-sticky ancestor permits replacing the private directory.
        // Sticky root-owned /tmp is safe: other users cannot rename our entries.
        if (metadata.uid() != 0 && metadata.uid() != user_id())
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "control socket ancestors must be root/user-owned and not writable by other users",
            ));
        }
    }
    let metadata = fs::symlink_metadata(parent)?;
    if !private(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control socket parent must be user-owned with no group/other permissions (0700)",
        ));
    }
    Ok(())
}

pub fn validate_socket(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || !private(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control socket must be a private user-owned Unix socket",
        ));
    }
    Ok(())
}

pub struct OwnedSocket {
    pub listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: File,
}

impl OwnedSocket {
    pub fn bind(path: &Path, create_parent: bool) -> io::Result<Self> {
        if create_parent {
            let parent = path
                .parent()
                .ok_or_else(|| invalid("control socket has no parent"))?;
            validate_parent(parent)?;
            match DirBuilder::new().mode(0o700).create(parent) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        validate_parent(path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path.with_extension("lock"))?;
        let metadata = lock.metadata()?;
        if !metadata.is_file() || !private(&metadata) || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "engine lock must be a private user-owned regular file with one link",
            ));
        }
        // SAFETY: flock operates on the live owned lock descriptor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "an engine already owns this control socket",
                ));
            }
            return Err(error);
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                // Reclaim only our user's sockets after acquiring the lifetime lock.
                // A crash between bind and chmod can leave a permissive socket inode,
                // but its private parent still prevents access by another user.
                if !metadata.file_type().is_socket() || metadata.uid() != user_id() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "refusing to replace a non-socket or another user's socket",
                    ));
                }
                fs::remove_file(path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let owned = Self {
            listener,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            _lock: lock,
        };
        fs::set_permissions(path, Permissions::from_mode(0o600))?;
        owned.listener.set_nonblocking(true)?;
        Ok(owned)
    }
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct PrivateDirectory(PathBuf);

    impl PrivateDirectory {
        fn new() -> Self {
            // Rootless Toolbox can expose /tmp as uid 65534; keep production
            // ownership checks intact and use the user's runtime or root-owned /var/tmp.
            let base = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/tmp"));
            let path = base.join(format!(
                "cosmos-socket-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for PrivateDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn lifetime_lock_prevents_duplicates_and_preserves_lock_inode() {
        let directory = PrivateDirectory::new();
        let path = directory.0.join("control.sock");
        let first = OwnedSocket::bind(&path, false).unwrap();
        let inode = fs::metadata(path.with_extension("lock")).unwrap().ino();
        assert_eq!(
            OwnedSocket::bind(&path, false).err().unwrap().kind(),
            io::ErrorKind::AddrInUse
        );
        drop(first);
        let _second = OwnedSocket::bind(&path, false).unwrap();
        assert_eq!(
            fs::metadata(path.with_extension("lock")).unwrap().ino(),
            inode
        );
    }

    #[test]
    fn stale_socket_can_be_reclaimed_but_regular_files_cannot() {
        let directory = PrivateDirectory::new();
        let path = directory.0.join("control.sock");
        let stale = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        drop(OwnedSocket::bind(&path, false).unwrap());
        fs::write(&path, "preserve this").unwrap();
        assert!(OwnedSocket::bind(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "preserve this");
    }

    #[test]
    fn socket_and_parent_symlinks_are_rejected() {
        let directory = PrivateDirectory::new();
        let path = directory.0.join("control.sock");
        symlink("missing", &path).unwrap();
        assert!(OwnedSocket::bind(&path, false).is_err());
        let alias = directory.0.join("alias");
        symlink(&directory.0, &alias).unwrap();
        assert!(validate_parent(&alias.join("other.sock")).is_err());
    }

    #[test]
    fn permissive_directory_is_rejected_without_chmod() {
        let directory = PrivateDirectory::new();
        fs::set_permissions(&directory.0, Permissions::from_mode(0o755)).unwrap();
        assert!(OwnedSocket::bind(&directory.0.join("control.sock"), false).is_err());
        assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o755);
    }
}
