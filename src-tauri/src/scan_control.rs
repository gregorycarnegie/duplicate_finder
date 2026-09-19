use std::{
    collections::HashMap,
    fs::{self, Metadata},
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::SystemTime,
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

#[cfg(test)]
mod tests {
    use super::*;
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
