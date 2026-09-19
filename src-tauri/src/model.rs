use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

fn recompute_group(group: &mut DuplicateGroup) {
    let total: u64 = group.files.iter().map(|f| f.entry.size).sum();
    let max = group.files.iter().map(|f| f.entry.size).max().unwrap_or(0);
    group.reclaimable_bytes = total.saturating_sub(max);
}

/// Drops the given paths from every group (used after a trash/delete
/// operation), removing groups that no longer have a duplicate partner and
/// recomputing reclaimable byte counts to match.
pub fn remove_paths(summary: &mut ScanSummary, removed: &HashSet<String>) {
    for group in summary
        .exact_groups
        .iter_mut()
        .chain(summary.media_groups.iter_mut())
    {
        group.files.retain(|f| !removed.contains(&f.entry.path));
        recompute_group(group);
    }
    summary.exact_groups.retain(|g| g.files.len() > 1);
    summary.media_groups.retain(|g| g.files.len() > 1);
    summary.reclaimable_bytes = summary
        .exact_groups
        .iter()
        .chain(summary.media_groups.iter())
        .map(|g| g.reclaimable_bytes)
        .sum();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn file(path: &str, size: u64) -> DuplicateFile {
        DuplicateFile {
            entry: FileEntry {
                path: path.to_string(),
                size,
                modified: None,
            },
            media: None,
        }
    }

    #[test]
    fn removing_a_path_shrinks_the_group_and_reclaimable_total() {
        let mut summary = ScanSummary {
            files_scanned: 3,
            bytes_scanned: 300,
            exact_groups: vec![DuplicateGroup {
                files: vec![file("a", 100), file("b", 100), file("c", 100)],
                reclaimable_bytes: 200,
            }],
            media_groups: vec![],
            reclaimable_bytes: 200,
            elapsed_ms: 0,
            ffmpeg_available: false,
        };

        remove_paths(&mut summary, &HashSet::from(["b".to_string()]));

        assert_eq!(summary.exact_groups[0].files.len(), 2);
        assert_eq!(summary.exact_groups[0].reclaimable_bytes, 100);
        assert_eq!(summary.reclaimable_bytes, 100);
    }

    #[test]
    fn removing_down_to_one_file_drops_the_whole_group() {
        let mut summary = ScanSummary {
            files_scanned: 2,
            bytes_scanned: 200,
            exact_groups: vec![DuplicateGroup {
                files: vec![file("a", 100), file("b", 100)],
                reclaimable_bytes: 100,
            }],
            media_groups: vec![],
            reclaimable_bytes: 100,
            elapsed_ms: 0,
            ffmpeg_available: false,
        };

        remove_paths(&mut summary, &HashSet::from(["b".to_string()]));

        assert!(summary.exact_groups.is_empty());
        assert_eq!(summary.reclaimable_bytes, 0);
    }

    fn summary_from_group_sizes(group_sizes: &[Vec<u64>]) -> ScanSummary {
        let mut idx = 0usize;
        let exact_groups: Vec<DuplicateGroup> = group_sizes
            .iter()
            .map(|sizes| {
                let files: Vec<DuplicateFile> = sizes
                    .iter()
                    .map(|&size| {
                        let path = format!("f{idx}");
                        idx += 1;
                        file(&path, size)
                    })
                    .collect();
                let total: u64 = sizes.iter().sum();
                let max = sizes.iter().copied().max().unwrap_or(0);
                DuplicateGroup {
                    files,
                    reclaimable_bytes: total - max,
                }
            })
            .collect();
        let reclaimable_bytes = exact_groups.iter().map(|g| g.reclaimable_bytes).sum();

        ScanSummary {
            files_scanned: idx as u64,
            bytes_scanned: 0,
            exact_groups,
            media_groups: vec![],
            reclaimable_bytes,
            elapsed_ms: 0,
            ffmpeg_available: false,
        }
    }

    proptest! {
        #[test]
        fn remove_paths_upholds_its_invariants(
            group_sizes in prop::collection::vec(prop::collection::vec(1u64..1_000, 2..6), 0..5),
            removal_mask in prop::collection::vec(any::<bool>(), 0..30),
        ) {
            let mut summary = summary_from_group_sizes(&group_sizes);
            let all_paths: Vec<String> = summary
                .exact_groups
                .iter()
                .flat_map(|g| g.files.iter().map(|f| f.entry.path.clone()))
                .collect();
            let removed: HashSet<String> = all_paths
                .iter()
                .zip(removal_mask.iter().chain(std::iter::repeat(&false)))
                .filter(|&(_, &r)| r)
                .map(|(p, _)| p.clone())
                .collect();

            remove_paths(&mut summary, &removed);

            let mut expected_total = 0u64;
            for group in &summary.exact_groups {
                prop_assert!(group.files.len() > 1);
                for f in &group.files {
                    prop_assert!(!removed.contains(&f.entry.path));
                }
                let total: u64 = group.files.iter().map(|f| f.entry.size).sum();
                let max = group.files.iter().map(|f| f.entry.size).max().unwrap();
                prop_assert_eq!(group.reclaimable_bytes, total - max);
                expected_total += group.reclaimable_bytes;
            }
            prop_assert_eq!(summary.reclaimable_bytes, expected_total);
        }
    }
}
