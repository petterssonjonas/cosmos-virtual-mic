// SPDX-License-Identifier: MPL-2.0

use std::ffi::CString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cosmos_control::{self as control, Profile, model::MAX_PROFILE_BYTES};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn default_path() -> io::Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| control::invalid("HOME is unset; use --config"))?,
        )
        .join(".config"),
    };
    if !base.is_absolute() {
        return Err(control::invalid("configuration home must be absolute"));
    }
    Ok(base.join("cosmos-audio-mixer/profile.json"))
}

fn user_id() -> u32 {
    // SAFETY: geteuid has no pointer or lifetime requirements.
    unsafe { libc::geteuid() }
}

fn prepare_parent(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(control::invalid("config must be an absolute file path"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| control::invalid("config has no parent"))?;
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component),
            _ => return Err(control::invalid("config path must not contain . or ..")),
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                DirBuilder::new().mode(0o700).create(&current)?;
                fs::symlink_metadata(&current)?
            }
            Err(error) => return Err(error),
        };
        if !metadata.is_dir()
            || (metadata.uid() != 0 && metadata.uid() != user_id())
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
        {
            return Err(control::invalid(
                "config ancestors must be symlink-free trusted directories",
            ));
        }
    }
    control::socket::validate_parent(path)
}

fn open_private(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != user_id()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(control::invalid(
            "config must be a private user-owned regular file with one link",
        ));
    }
    Ok(file)
}

/// Bounded reading also applies to CLI imports; arbitrary non-regular endpoints are rejected.
pub fn read_profile(path: &Path) -> io::Result<Profile> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(control::invalid("profile import must be a regular file"));
    }
    decode_file(file)
}

fn decode_file(file: File) -> io::Result<Profile> {
    if file.metadata()?.len() > MAX_PROFILE_BYTES as u64 {
        return Err(control::invalid("profile exceeds byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Profile::decode(&bytes)
}

fn preserve_invalid(path: &Path, backup: &Path, directory: &File) -> io::Result<()> {
    open_private(path)?.sync_all()?;
    let source = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| control::invalid("config path contains NUL"))?;
    let target = CString::new(backup.as_os_str().as_encoded_bytes())
        .map_err(|_| control::invalid("config backup path contains NUL"))?;
    // SAFETY: both paths are live NUL-terminated strings. RENAME_NOREPLACE
    // preserves any existing backup and never adds a second link to the config.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    directory.sync_all()
}

fn missing_profile_error(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| control::invalid("config has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| control::invalid("config has no name"))?;
    let prefix = format!("{}.invalid-", name.to_string_lossy());
    // A crash after preserving corrupt input may leave only its backup. Do not
    // silently describe that state as a first launch. Bound recovery discovery.
    for (index, entry) in fs::read_dir(parent)?.enumerate() {
        if index >= 512 {
            return Err(control::invalid(
                "config is missing; recovery scan exceeded entry limit",
            ));
        }
        if entry?
            .file_name()
            .as_encoded_bytes()
            .starts_with(prefix.as_bytes())
        {
            return Err(control::invalid(
                "config is missing; preserved invalid backup remains in its directory",
            ));
        }
    }
    Ok(())
}

pub struct Store {
    path: PathBuf,
    pub profile: Profile,
    pub error: Option<String>,
    lock: Option<File>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            profile: Profile::default(),
            error: None,
            lock: None,
        };
        if let Err(error) = store.initialize() {
            store.error = Some(error.to_string());
        }
        store
    }

    fn initialize(&mut self) -> io::Result<()> {
        prepare_parent(&self.path)?;
        self.acquire_lock()?;
        match open_private(&self.path) {
            Ok(file) => self.profile = decode_file(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing_profile_error(&self.path)?
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn acquire_lock(&mut self) -> io::Result<()> {
        if self.lock.is_some() {
            return Ok(());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(self.path.with_extension("lock"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != user_id()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(control::invalid(
                "profile lock must be a private user-owned regular file with one link",
            ));
        }
        // SAFETY: flock operates only on the live owned descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.lock = Some(file);
        Ok(())
    }

    pub fn apply(&mut self, profile: Profile) -> io::Result<()> {
        profile.validate()?;
        match self.save(&profile) {
            Ok(sync_error) => {
                self.profile = profile;
                self.error = sync_error;
                Ok(())
            }
            Err(error) => {
                self.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    // Once rename succeeds desired state is accepted even if the directory fsync fails;
    // report that durability error separately rather than claim the old state is retained.
    fn save(&mut self, profile: &Profile) -> io::Result<Option<String>> {
        prepare_parent(&self.path)?;
        self.acquire_lock()?;
        let bytes = control::encode(profile, MAX_PROFILE_BYTES)?;
        let existing = match open_private(&self.path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let invalid_original = existing.map(decode_file).transpose().is_err();
        let parent = self
            .path
            .parent()
            .ok_or_else(|| control::invalid("config has no parent"))?;
        let directory = File::open(parent)?;
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let name = self
            .path
            .file_name()
            .ok_or_else(|| control::invalid("config has no name"))?
            .to_string_lossy();
        let temporary = parent.join(format!(".{name}.tmp-{suffix}"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            if invalid_original {
                let backup = parent.join(format!("{name}.invalid-{suffix}"));
                // Moving rather than hard-linking keeps the original recoverable
                // after a crash or error between backup and replacement.
                preserve_invalid(&self.path, &backup, &directory)?;
            }
            fs::rename(&temporary, &self.path)?;
            Ok(directory
                .sync_all()
                .err()
                .map(|error| format!("profile accepted but directory sync failed: {error}")))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_original_survives_explicit_apply_and_restart() {
        let path = PathBuf::from("/var/tmp").join(format!(
            "cosmos-profile-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        let config = path.join("profile.json");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&config)
            .unwrap()
            .write_all(b"broken original")
            .unwrap();
        let mut store = Store::load(config.clone());
        assert!(store.error.is_some());
        store.apply(Profile::default()).unwrap();
        assert!(store.error.is_none());
        let saved = fs::read(&config).unwrap();
        let invalid = Profile {
            version: control::model::PROFILE_VERSION + 1,
            ..Profile::default()
        };
        assert!(store.apply(invalid).is_err());
        assert_eq!(fs::read(&config).unwrap(), saved);
        assert!(fs::read_dir(&path).unwrap().any(|entry| {
            let entry = entry.unwrap();
            entry.file_name().to_string_lossy().contains(".invalid-")
                && fs::read(entry.path()).unwrap() == b"broken original"
        }));
        drop(store);
        assert!(Store::load(config).error.is_none());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn interrupted_invalid_backup_allows_restart_and_future_apply() {
        let path = PathBuf::from("/var/tmp").join(format!(
            "cosmos-profile-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        let config = path.join("profile.json");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&config)
            .unwrap()
            .write_all(b"preserve interrupted original")
            .unwrap();
        let store = Store::load(config.clone());
        let backup = path.join("profile.json.invalid-interrupted");
        preserve_invalid(&config, &backup, &File::open(&path).unwrap()).unwrap();
        drop(store);
        assert!(!config.exists());
        assert_eq!(fs::metadata(&backup).unwrap().nlink(), 1);
        let mut restarted = Store::load(config.clone());
        assert!(restarted.error.is_some());
        restarted.apply(Profile::default()).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"preserve interrupted original");
        drop(restarted);
        assert!(Store::load(config).error.is_none());
        fs::remove_dir_all(path).unwrap();
    }
}
