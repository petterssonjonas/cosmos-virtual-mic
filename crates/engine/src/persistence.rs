// SPDX-License-Identifier: MPL-2.0

use std::ffi::CString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cosmos_control::{self as control, Profile, model::MAX_PROFILE_BYTES};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SAVE_DELAY: Duration = Duration::from_secs(2);

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
    pending: bool,
    save_at: Option<Instant>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            profile: Profile::default(),
            error: None,
            lock: None,
            pending: false,
            save_at: None,
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

    pub fn report_status(&self, status: &mut control::Status) {
        status.config_pending = self.pending;
        if status.config_error != self.error {
            status.config_error.clone_from(&self.error);
        }
    }

    pub fn update(&mut self, profile: Profile, now: Instant) -> io::Result<()> {
        profile.validate()?;
        if self.profile != profile {
            self.profile = profile;
            self.pending = true;
            self.save_at = Some(now + SAVE_DELAY);
        } else if self.error.is_some() && self.save_at.is_none() {
            // An explicit repeated edit can retry a failed save, but status polling
            // and unchanged values must not keep moving an active idle deadline.
            self.pending = true;
            self.save_at = Some(now + SAVE_DELAY);
        }
        Ok(())
    }

    pub fn apply(&mut self, profile: Profile) -> io::Result<()> {
        profile.validate()?;
        let result = self.prepare_write().and_then(|()| self.save(&profile));
        if result.is_ok() {
            self.profile = profile;
            self.pending = true;
        }
        self.finish_save(result)
    }

    pub fn save_if_due(&mut self, now: Instant) -> io::Result<()> {
        if self.save_at.is_some_and(|deadline| now >= deadline) {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if !self.pending {
            // A startup read failure is not permission to overwrite its fallback.
            return self
                .error
                .as_ref()
                .map_or(Ok(()), |error| Err(io::Error::other(error.clone())));
        }
        let result = self.prepare_write().and_then(|()| self.save(&self.profile));
        self.finish_save(result)
    }

    pub fn flush_on_shutdown(&mut self) -> io::Result<()> {
        if self.pending {
            self.flush()?;
        }
        Ok(())
    }

    fn finish_save(&mut self, result: io::Result<Option<String>>) -> io::Result<()> {
        // Failed automatic saves wait for a new edit or an explicit flush, not
        // another attempt on every engine tick.
        self.save_at = None;
        let result = result.and_then(|sync_error| match sync_error {
            Some(message) => Err(io::Error::other(message)),
            None => Ok(()),
        });
        match result {
            Ok(()) => {
                self.pending = false;
                self.error = None;
                Ok(())
            }
            Err(error) => {
                self.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    fn prepare_write(&mut self) -> io::Result<()> {
        prepare_parent(&self.path)?;
        self.acquire_lock()
    }

    // Once rename succeeds desired state is accepted even if the directory fsync fails;
    // report that durability error separately rather than claim the old state is retained.
    fn save(&self, profile: &Profile) -> io::Result<Option<String>> {
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
    use control::{Bus, Channels, DeviceSettings, Effect, EffectInstance, NodeRef, Route};

    fn temporary_config() -> PathBuf {
        let directory = PathBuf::from("/var/tmp").join(format!(
            "cosmos-profile-debounce-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        directory.join("profile.json")
    }

    fn complete_profile(gain_db: f32) -> Profile {
        let identity =
            std::collections::BTreeMap::from([("node.name".into(), "fixture-source".into())]);
        Profile {
            version: control::model::PROFILE_VERSION,
            buses: vec![Bus {
                id: "voice".into(),
                name: "Saved voice".into(),
                channels: Channels::Stereo,
                enabled: false,
                hidden: true,
                muted: true,
                gain_db,
                effects: vec![
                    EffectInstance {
                        id: "gate".into(),
                        enabled: false,
                        effect: Effect::Gate {
                            threshold_db: -42.0,
                            attack_ms: 5.0,
                            hold_ms: 15.0,
                            release_ms: 60.0,
                        },
                    },
                    EffectInstance {
                        id: "compressor".into(),
                        enabled: true,
                        effect: Effect::Compressor {
                            threshold_db: -18.0,
                            ratio: 3.0,
                            attack_ms: 10.0,
                            release_ms: 100.0,
                            makeup_db: 2.0,
                        },
                    },
                    EffectInstance {
                        id: "color-low".into(),
                        enabled: true,
                        effect: Effect::Equalizer {
                            frequency_hz: 150.0,
                            gain_db: 3.0,
                            q: 0.7,
                        },
                    },
                    EffectInstance {
                        id: "color-mid".into(),
                        enabled: true,
                        effect: Effect::Equalizer {
                            frequency_hz: 1200.0,
                            gain_db: -2.0,
                            q: 1.0,
                        },
                    },
                    EffectInstance {
                        id: "color-high".into(),
                        enabled: false,
                        effect: Effect::Equalizer {
                            frequency_hz: 8000.0,
                            gain_db: 4.0,
                            q: 0.8,
                        },
                    },
                    EffectInstance {
                        id: "limiter".into(),
                        enabled: true,
                        effect: Effect::Limiter {
                            ceiling_db: -1.0,
                            release_ms: 5.0,
                        },
                    },
                ],
            }],
            devices: vec![DeviceSettings {
                identity: identity.clone(),
                alias: "Saved hardware".into(),
                enabled: false,
                hidden: true,
                muted: true,
            }],
            routes: vec![Route {
                id: "input".into(),
                from: NodeRef::Hardware { identity },
                to: NodeRef::Bus { id: "voice".into() },
                enabled: false,
            }],
        }
    }

    #[test]
    fn updates_save_complete_latest_state_only_after_two_idle_seconds() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        store.apply(Profile::default()).unwrap();
        let original = fs::read(&config).unwrap();
        let now = Instant::now();
        let first = complete_profile(-6.0);
        store.update(first.clone(), now).unwrap();
        assert_eq!(store.profile, first);
        assert!(store.pending);
        store
            .save_if_due(now + Duration::from_millis(1499))
            .unwrap();
        assert_eq!(fs::read(&config).unwrap(), original);
        let latest = complete_profile(-12.0);
        store
            .update(latest.clone(), now + Duration::from_millis(1500))
            .unwrap();
        store.save_if_due(now + SAVE_DELAY).unwrap();
        assert_eq!(fs::read(&config).unwrap(), original);
        // Repeated values do not defer the changed value's idle deadline.
        store
            .update(latest.clone(), now + Duration::from_secs(3))
            .unwrap();
        store
            .save_if_due(now + Duration::from_millis(3499))
            .unwrap();
        assert_eq!(fs::read(&config).unwrap(), original);
        store
            .save_if_due(now + Duration::from_millis(3500))
            .unwrap();
        assert!(!store.pending);
        assert_eq!(fs::metadata(&config).unwrap().mode() & 0o777, 0o600);
        drop(store);
        let restored = Store::load(config.clone());
        assert_eq!(restored.profile, latest);
        assert!(restored.error.is_none());
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn unchanged_defaults_do_not_create_a_profile_or_startup_autosave() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        let now = Instant::now();
        store.update(Profile::default(), now).unwrap();
        store.save_if_due(now + SAVE_DELAY).unwrap();
        store.flush_on_shutdown().unwrap();
        assert!(!store.pending);
        assert!(!config.exists());
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_autosave_retains_live_state_and_waits_for_explicit_recovery() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        store.apply(Profile::default()).unwrap();
        let original = fs::read(&config).unwrap();
        let previous = config.with_extension("previous");
        let desired = complete_profile(-9.0);
        let now = Instant::now();
        store.update(desired.clone(), now).unwrap();
        fs::rename(&config, &previous).unwrap();
        fs::create_dir(&config).unwrap();
        assert!(store.save_if_due(now + SAVE_DELAY).is_err());
        let failure = store.error.clone();
        assert!(failure.is_some());
        assert!(store.pending);
        assert_eq!(store.profile, desired);
        // Failed Apply must not replace already accepted unsaved desired state.
        assert!(store.apply(complete_profile(-3.0)).is_err());
        assert_eq!(store.profile, desired);
        assert!(store.flush_on_shutdown().is_err());
        fs::remove_dir(&config).unwrap();
        fs::rename(&previous, &config).unwrap();
        store.save_if_due(now + Duration::from_secs(30)).unwrap();
        assert_eq!(fs::read(&config).unwrap(), original);
        assert!(store.error.is_some());
        assert!(store.pending);
        store.flush().unwrap();
        assert!(!store.pending);
        assert!(store.error.is_none());
        assert_eq!(read_profile(&config).unwrap(), desired);
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn repeated_edit_rearms_failed_save_without_losing_writer_lock_guarantees() {
        let config = temporary_config();
        let mut owner = Store::load(config.clone());
        owner.apply(Profile::default()).unwrap();
        let mut store = Store::load(config.clone());
        assert!(store.error.is_some());
        let desired = complete_profile(-8.0);
        let now = Instant::now();
        store.update(desired.clone(), now).unwrap();
        assert!(store.save_if_due(now + SAVE_DELAY).is_err());
        assert_eq!(read_profile(&config).unwrap(), Profile::default());
        drop(owner);
        let retry = now + Duration::from_secs(5);
        store.update(desired.clone(), retry).unwrap();
        store
            .save_if_due(retry + Duration::from_millis(1999))
            .unwrap();
        assert_eq!(read_profile(&config).unwrap(), Profile::default());
        store.save_if_due(retry + SAVE_DELAY).unwrap();
        assert_eq!(read_profile(&config).unwrap(), desired);
        assert!(!store.pending);
        assert!(store.error.is_none());
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn unreadable_startup_is_not_saved_until_an_explicit_edit_and_backup_survives() {
        let config = temporary_config();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&config)
            .unwrap()
            .write_all(b"preserve original")
            .unwrap();
        let mut store = Store::load(config.clone());
        let now = Instant::now();
        store.save_if_due(now + SAVE_DELAY).unwrap();
        assert!(store.flush().is_err());
        store.flush_on_shutdown().unwrap();
        assert_eq!(fs::read(&config).unwrap(), b"preserve original");
        store.update(Profile::default(), now).unwrap();
        store.save_if_due(now + SAVE_DELAY).unwrap();
        assert_eq!(read_profile(&config).unwrap(), Profile::default());
        assert!(
            fs::read_dir(config.parent().unwrap())
                .unwrap()
                .any(|entry| {
                    let path = entry.unwrap().path();
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(".invalid-")
                        && fs::read(path).unwrap() == b"preserve original"
                })
        );
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn shutdown_flushes_the_latest_complete_profile_before_the_idle_deadline() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        let now = Instant::now();
        store.update(complete_profile(-4.0), now).unwrap();
        let latest = complete_profile(-11.0);
        store.update(latest.clone(), now).unwrap();
        assert!(!config.exists());
        store.flush_on_shutdown().unwrap();
        assert_eq!(read_profile(&config).unwrap(), latest);
        assert!(!store.pending);
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn directory_sync_uncertainty_is_not_acknowledged_as_durable() {
        let config = temporary_config();
        let mut store = Store::load(config.clone());
        let desired = complete_profile(-7.0);
        store.update(desired.clone(), Instant::now()).unwrap();
        assert!(
            store
                .finish_save(Ok(Some(
                    "profile accepted but directory sync failed".into()
                )))
                .is_err()
        );
        assert!(store.pending);
        assert!(store.error.is_some());
        assert_eq!(store.profile, desired);
        store.flush().unwrap();
        assert_eq!(read_profile(&config).unwrap(), desired);
        assert!(!store.pending);
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

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
