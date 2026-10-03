use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub struct TestDir(pub PathBuf);
impl TestDir {
    pub fn new() -> Self {
        Self::in_dir(&std::env::temp_dir())
    }
    pub fn in_dir(parent: &std::path::Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = parent.join(format!(
            "duplicate-finder-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub fn file(&self, name: &str, contents: &[u8]) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_string()
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
