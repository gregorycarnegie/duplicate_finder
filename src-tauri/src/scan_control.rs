use std::{
    collections::HashMap,
    fs::{self, Metadata},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

#[derive(Default)]
pub struct Operations {
    busy: Arc<AtomicBool>,
    pub cancelled: Arc<AtomicBool>,
}

pub struct OperationGuard(Arc<AtomicBool>);
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
impl Operations {
    pub fn begin(&self) -> Result<OperationGuard, String> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "Another scan or file operation is still running.".to_string())?;
        self.cancelled.store(false, Ordering::SeqCst);
        Ok(OperationGuard(self.busy.clone()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}
impl FileStamp {
    pub fn read(path: &str) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        Self::from_metadata(&metadata)
    }
    pub fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        if !metadata.is_file() {
            return Err(io::Error::other("Path is no longer a regular file"));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            size: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[derive(Clone)]
pub struct FileSnapshot {
    pub stamp: FileStamp,
    pub hash: blake3::Hash,
}

#[derive(Default)]
pub struct ScanControl {
    pub cancelled: Arc<AtomicBool>,
    pub warnings: Mutex<Vec<String>>,
    pub stamps: Mutex<HashMap<String, FileStamp>>,
    pub hashes: Mutex<HashMap<String, blake3::Hash>>,
}
impl ScanControl {
    pub fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Scan cancelled.",
            ))
        } else {
            Ok(())
        }
    }
    pub fn warn(&self, path: &str, error: impl std::fmt::Display) {
        self.warnings
            .lock()
            .unwrap()
            .push(format!("{path}: {error}"));
    }
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.lock().unwrap().clone()
    }
}

// Hash workers may finish out of order. Bound IPC traffic while keeping the
// displayed progress monotonic and always delivering completion.
#[derive(Default)]
pub struct ProgressGate(Mutex<Option<(&'static str, u64, Instant)>>);
impl ProgressGate {
    pub fn allow(&self, phase: &'static str, done: u64, total: u64) -> bool {
        self.allow_at(phase, done, total, Instant::now())
    }
    fn allow_at(&self, phase: &'static str, done: u64, total: u64, now: Instant) -> bool {
        let mut last = self.0.lock().unwrap();
        if let Some((last_phase, last_done, last_time)) = *last
            && phase == last_phase
        {
            if done <= last_done {
                return false;
            }
            if done != total && now.duration_since(last_time) < Duration::from_millis(100) {
                return false;
            }
        }
        *last = Some((phase, done, now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_throttled_monotonic_and_finishes() {
        let gate = ProgressGate::default();
        let now = Instant::now();
        assert!(gate.allow_at("hash", 0, 100, now));
        assert!(!gate.allow_at("hash", 40, 100, now));
        assert!(gate.allow_at("hash", 60, 100, now + Duration::from_millis(100)));
        assert!(!gate.allow_at("hash", 50, 100, now + Duration::from_millis(200)));
        assert!(gate.allow_at("hash", 100, 100, now + Duration::from_millis(110)));
        assert!(gate.allow_at("compare", 0, 100, now + Duration::from_millis(111)));
    }
    #[test]
    fn scan_and_delete_operations_are_exclusive_and_unlock_after_failure() {
        let operations = Operations::default();
        let guard = operations.begin().unwrap();
        assert!(operations.begin().is_err());
        operations.cancelled.store(true, Ordering::SeqCst);
        drop(guard);
        let _guard = operations.begin().unwrap();
        assert!(!operations.cancelled.load(Ordering::SeqCst));
    }
}
