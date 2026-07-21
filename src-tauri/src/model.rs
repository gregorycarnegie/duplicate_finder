use serde::{Deserialize, Serialize};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub modified: Option<i64>,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub duration_secs: f64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codec: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    #[serde(flatten)]
    pub entry: FileEntry,
    pub media: Option<MediaInfo>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub files: Vec<DuplicateFile>,
    pub reclaimable_bytes: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub files_scanned: u64,
    pub bytes_scanned: u64,
    pub exact_groups: Vec<DuplicateGroup>,
    pub media_groups: Vec<DuplicateGroup>,
    pub reclaimable_bytes: u64,
    pub elapsed_ms: u64,
    pub ffmpeg_available: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    pub folders: Vec<String>,
    pub duration_tolerance_secs: f64,
    pub min_file_size: u64,
    pub include_hidden: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase", tag = "phase")]
pub enum ScanProgress {
    Walking { folder: String, files_found: u64 },
    Hashing { done: u64, total: u64 },
    Probing { done: u64, total: u64 },
}
