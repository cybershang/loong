use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn subprocess_lock() -> &'static Mutex<()> {
    static SUBPROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    SUBPROCESS_LOCK.get_or_init(|| Mutex::new(()))
}

thread_local! {
    static SUBPROCESS_TEST_DEPTH: Cell<usize> = const { Cell::new(0) };
}

fn subprocess_test_depth() -> usize {
    SUBPROCESS_TEST_DEPTH.with(|depth| depth.get())
}

fn set_subprocess_test_depth(value: usize) {
    SUBPROCESS_TEST_DEPTH.with(|depth| depth.set(value));
}

pub struct ScopedEnv {
    originals: Vec<(&'static str, Option<OsString>)>,
    guard: Option<MutexGuard<'static, ()>>,
}

impl ScopedEnv {
    pub fn new() -> Self {
        let depth_before = scoped_env_depth();
        let guard = if depth_before == 0 {
            let guard = crate::test_support::lock_process_env_for_tests();
            Some(guard)
        } else {
            None
        };
        set_scoped_env_depth(depth_before.saturating_add(1));
        Self {
            originals: Vec::new(),
            guard,
        }
    }

    #[allow(clippy::disallowed_methods)]
    pub fn set(&mut self, key: &'static str, value: impl AsRef<OsStr>) {
        self.capture_original(key);
        crate::process_env::set_var(key, value);
    }

    #[allow(dead_code, clippy::disallowed_methods)]
    pub fn remove(&mut self, key: &'static str) {
        self.capture_original(key);
        crate::process_env::remove_var(key);
    }

    fn capture_original(&mut self, key: &'static str) {
        if self.originals.iter().any(|(saved, _)| *saved == key) {
            return;
        }
        if default_loong_home_env_override_key(key) {
            crate::config::push_default_loong_home_env_override_for_tests();
        }
        self.originals.push((key, std::env::var_os(key)));
    }
}

impl Drop for ScopedEnv {
    #[allow(clippy::disallowed_methods)]
    fn drop(&mut self) {
        for (key, original) in self.originals.iter().rev() {
            match original {
                Some(value) => crate::process_env::set_var(key, value),
                None => crate::process_env::remove_var(key),
            }
            if default_loong_home_env_override_key(key) {
                crate::config::pop_default_loong_home_env_override_for_tests();
            }
        }

        let depth_before = scoped_env_depth();
        let depth_after = depth_before.saturating_sub(1);
        set_scoped_env_depth(depth_after);

        if depth_after == 0 {
            self.guard.take();
        }
    }
}

fn default_loong_home_env_override_key(key: &str) -> bool {
    matches!(key, "HOME" | "USERPROFILE" | "LOONG_HOME")
}

thread_local! {
    static SCOPED_ENV_DEPTH: Cell<usize> = const { Cell::new(0) };
}

fn scoped_env_depth() -> usize {
    SCOPED_ENV_DEPTH.with(|depth| depth.get())
}

fn set_scoped_env_depth(value: usize) {
    SCOPED_ENV_DEPTH.with(|depth| {
        depth.set(value);
    });
}

pub struct ScopedLoongHome {
    _temp_home: Option<tempfile::TempDir>,
    path: PathBuf,
}

impl ScopedLoongHome {
    pub fn new(prefix: &str) -> Self {
        let temp_home = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create scoped loong home");
        let path = temp_home.path().to_path_buf();
        crate::config::push_default_loong_home_override_for_tests(path.clone());
        crate::tools::reset_runtime_home_state_for_tests();
        Self {
            _temp_home: Some(temp_home),
            path,
        }
    }

    pub fn from_existing(path: PathBuf) -> Self {
        crate::config::push_default_loong_home_override_for_tests(path.clone());
        crate::tools::reset_runtime_home_state_for_tests();
        Self {
            _temp_home: None,
            path,
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn join(&self, relative: impl AsRef<std::path::Path>) -> PathBuf {
        self.path().join(relative)
    }
}

impl Drop for ScopedLoongHome {
    fn drop(&mut self) {
        crate::tools::reset_runtime_home_state_for_tests();
        crate::config::pop_default_loong_home_override_for_tests();
    }
}

static TEST_TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn unique_temp_dir(prefix: &str) -> PathBuf {
    let id = TEST_TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
}

pub(crate) struct SubprocessTestGuard {
    guard: Option<MutexGuard<'static, ()>>,
}

pub(crate) fn acquire_subprocess_test_guard() -> SubprocessTestGuard {
    let depth_before = subprocess_test_depth();
    let guard = if depth_before == 0 {
        let guard = subprocess_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(guard)
    } else {
        None
    };
    set_subprocess_test_depth(depth_before.saturating_add(1));
    SubprocessTestGuard { guard }
}

impl Drop for SubprocessTestGuard {
    fn drop(&mut self) {
        let depth_before = subprocess_test_depth();
        let depth_after = depth_before.saturating_sub(1);
        set_subprocess_test_depth(depth_after);

        if depth_after == 0 {
            self.guard.take();
        }
    }
}

pub(crate) struct ScopedCurrentDir {
    original: PathBuf,
    _guard: SubprocessTestGuard,
}

impl ScopedCurrentDir {
    pub(crate) fn new(path: &std::path::Path) -> Self {
        let guard = acquire_subprocess_test_guard();
        let original = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self {
            original,
            _guard: guard,
        }
    }
}

impl Drop for ScopedCurrentDir {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).expect("restore current dir");
    }
}

pub(crate) fn durable_memory_flush_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(unix)]
pub(crate) fn write_executable_script_atomically(
    script_path: &std::path::Path,
    contents: &str,
) -> std::io::Result<()> {
    write_executable_script_atomically_with(script_path, |file| {
        std::io::Write::write_all(file, contents.as_bytes())
    })
}

#[cfg(unix)]
fn write_executable_script_atomically_with<F>(
    script_path: &std::path::Path,
    writer: F,
) -> std::io::Result<()>
where
    F: FnOnce(&mut std::fs::File) -> std::io::Result<()>,
{
    static NEXT_STAGING_FILE_SEED: AtomicU64 = AtomicU64::new(1);

    let Some(parent) = script_path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "script path `{}` has no parent directory",
                script_path.display()
            ),
        ));
    };
    let Some(file_name) = script_path.file_name().and_then(|name| name.to_str()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "script path `{}` has no UTF-8 file name",
                script_path.display()
            ),
        ));
    };

    let seed = NEXT_STAGING_FILE_SEED.fetch_add(1, Ordering::Relaxed);
    let staged_path = parent.join(format!(".{file_name}.{}.{seed}.tmp", std::process::id()));
    let mut staged_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staged_path)?;
    let write_result = writer(&mut staged_file).and_then(|()| staged_file.sync_all());
    drop(staged_file);

    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&staged_path);
        return Err(error);
    }

    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(&staged_path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&staged_path, permissions)?;
    if let Err(error) = std::fs::rename(&staged_path, script_path) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ScopedEnv, unique_temp_dir};

    #[cfg(unix)]
    use super::{write_executable_script_atomically, write_executable_script_atomically_with};

    #[test]
    fn scoped_env_recovers_after_mutex_poison() {
        let panic_result = std::thread::spawn(|| {
            let _env = ScopedEnv::new();
            panic!("poison env lock for test");
        })
        .join();

        assert!(panic_result.is_err(), "setup thread should poison the lock");

        let recovery = std::panic::catch_unwind(ScopedEnv::new);
        assert!(
            recovery.is_ok(),
            "ScopedEnv::new should recover from a poisoned env lock"
        );
    }

    #[test]
    fn scoped_env_supports_nested_guards_on_one_thread() {
        let mut outer = ScopedEnv::new();
        outer.set("LOONG_HOME", "/tmp/outer");

        let mut inner = ScopedEnv::new();
        inner.set("LOONG_HOME", "/tmp/inner");

        let inner_value = std::env::var_os("LOONG_HOME");
        assert_eq!(inner_value, Some(std::ffi::OsString::from("/tmp/inner")));

        drop(inner);

        let outer_value = std::env::var_os("LOONG_HOME");
        assert_eq!(outer_value, Some(std::ffi::OsString::from("/tmp/outer")));
    }

    #[test]
    fn unique_temp_dir_uses_distinct_paths() {
        let first = unique_temp_dir("loong-test-support");
        let second = unique_temp_dir("loong-test-support");

        assert_ne!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn write_executable_script_atomically_preserves_existing_script_when_write_fails() {
        let root = unique_temp_dir("loong-test-support-script-write-failure");
        std::fs::create_dir_all(&root).expect("create temp dir");
        let script_path = root.join("fixture-script");

        write_executable_script_atomically(&script_path, "#!/bin/sh\necho old\n")
            .expect("write baseline script");

        let error = write_executable_script_atomically_with(&script_path, |_file| {
            Err(std::io::Error::other("forced write failure"))
        })
        .expect_err("failed staged write should surface an error");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            std::fs::read_to_string(&script_path).expect("baseline script should remain readable"),
            "#!/bin/sh\necho old\n"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
