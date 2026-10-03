use crate::model::{DuplicateFile, DuplicateGroup, FileEntry, MediaInfo};
use crate::scan_control::{FileStamp, ScanControl};
use rayon::prelude::*;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    sync::atomic::{AtomicU64, Ordering},
};

const CHUNK_SIZE: usize = 1024 * 1024;
// ponytail: 64 KiB at each end; increase only if real scans show frequent sample collisions.
const SAMPLE_SIZE: usize = 64 * 1024;

#[cfg(test)]
pub fn hash_file(path: &str) -> std::io::Result<blake3::Hash> {
    hash_file_controlled(path, &ScanControl::default())
}

/// Whether `path` still names the file `file` has open, both as stamped in
/// `before`, so the bytes just read belong to the file that was listed.
fn unchanged(path: &str, file: &File, before: &FileStamp) -> std::io::Result<bool> {
    Ok(
        FileStamp::read(path)? == *before
            && FileStamp::from_metadata(&file.metadata()?)? == *before,
    )
}

pub fn hash_file_controlled(path: &str, control: &ScanControl) -> std::io::Result<blake3::Hash> {
    control.check()?;
    let before = FileStamp::read(path)?;
    let file = File::open(path)?;
    if FileStamp::from_metadata(&file.metadata()?)? != before {
        return Err(std::io::Error::other(
            "File changed while opening; scan again",
        ));
    }
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        control.check()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if !unchanged(path, reader.get_ref(), &before)? {
        return Err(std::io::Error::other(
            "File changed while hashing; scan again",
        ));
    }
    Ok(hasher.finalize())
}

fn sample_hash(
    path: &str,
    size: u64,
    control: &ScanControl,
) -> std::io::Result<(blake3::Hash, bool)> {
    if size <= (SAMPLE_SIZE * 2) as u64 {
        return hash_file_controlled(path, control).map(|hash| (hash, true));
    }

    control.check()?;
    let before = FileStamp::read(path)?;
    let mut file = File::open(path)?;
    if FileStamp::from_metadata(&file.metadata()?)? != before {
        return Err(std::io::Error::other(
            "File changed while opening; scan again",
        ));
    }
    let mut hasher = blake3::Hasher::new();
    let mut sample = [0; SAMPLE_SIZE];
    file.read_exact(&mut sample)?;
    hasher.update(&sample);
    file.seek(SeekFrom::End(-(SAMPLE_SIZE as i64)))?;
    file.read_exact(&mut sample)?;
    hasher.update(&sample);
    if !unchanged(path, &file, &before)? {
        return Err(std::io::Error::other(
            "File changed while sampling; scan again",
        ));
    }
    Ok((hasher.finalize(), false))
}

/// Groups files that share an identical byte-for-byte content hash.
/// Only files that share a size with at least one other file are hashed,
/// since a unique size can never be an exact duplicate.
#[cfg(test)]
pub fn find_exact_duplicates<F: Fn(u64, u64) + Sync>(
    entries: &[FileEntry],
    media_lookup: &HashMap<String, MediaInfo>,
    on_progress: F,
) -> Vec<DuplicateGroup> {
    find_exact_duplicates_controlled(entries, media_lookup, on_progress, &ScanControl::default())
}

pub fn find_exact_duplicates_controlled<F: Fn(u64, u64) + Sync>(
    entries: &[FileEntry],
    media_lookup: &HashMap<String, MediaInfo>,
    on_progress: F,
    control: &ScanControl,
) -> Vec<DuplicateGroup> {
    let mut by_size: HashMap<u64, Vec<&FileEntry>> = HashMap::new();
    for e in entries {
        by_size.entry(e.size).or_default().push(e);
    }

    let candidates: Vec<&FileEntry> = by_size
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();

    let total = candidates.len() as u64;
    let sampled: Vec<(blake3::Hash, bool, &FileEntry)> = candidates
        .par_iter()
        .filter_map(|e| match sample_hash(&e.path, e.size, control) {
            Ok((hash, complete)) => {
                if complete {
                    control.hashes.lock().unwrap().insert(e.path.clone(), hash);
                }
                Some((hash, complete, *e))
            }
            Err(error) => {
                control.warn(&e.path, error);
                None
            }
        })
        .collect();

    let mut by_sample: HashMap<(u64, blake3::Hash, bool), Vec<&FileEntry>> = HashMap::new();
    for (hash, complete, entry) in sampled {
        by_sample
            .entry((entry.size, hash, complete))
            .or_default()
            .push(entry);
    }

    let mut hashed = Vec::new();
    let mut full_candidates = Vec::new();
    for ((_, hash, complete), files) in by_sample {
        if files.len() > 1 {
            if complete {
                hashed.extend(files.into_iter().map(|entry| (hash, entry)));
            } else {
                full_candidates.extend(files);
            }
        }
    }

    let done = AtomicU64::new(total - full_candidates.len() as u64);
    on_progress(done.load(Ordering::Relaxed), total);
    hashed.par_extend(full_candidates.par_iter().filter_map(|entry| {
        let result = match hash_file_controlled(&entry.path, control) {
            Ok(hash) => {
                control
                    .hashes
                    .lock()
                    .unwrap()
                    .insert(entry.path.clone(), hash);
                Some((hash, *entry))
            }
            Err(error) => {
                control.warn(&entry.path, error);
                None
            }
        };
        let done = done.fetch_add(1, Ordering::Relaxed) + 1;
        on_progress(done, total);
        result
    }));

    let mut by_hash: HashMap<(u64, blake3::Hash), Vec<&FileEntry>> = HashMap::new();
    for (hash, entry) in hashed {
        by_hash.entry((entry.size, hash)).or_default().push(entry);
    }

    by_hash
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|((size, _), files)| {
            let reclaimable_bytes = size * (files.len() as u64 - 1);
            DuplicateGroup {
                evidence: Default::default(),
                files: files
                    .iter()
                    .map(|e| DuplicateFile {
                        entry: (*e).clone(),
                        media: media_lookup.get(&e.path).cloned(),
                    })
                    .collect(),
                reclaimable_bytes,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::{fs, hint::black_box, io::Write, path::PathBuf, time::Instant};

    #[test]
    fn unreadable_candidates_are_reported() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.file("a", b"same");
        let missing = dir.0.join("missing").to_str().unwrap().to_string();
        let entries = vec![
            FileEntry {
                path,
                size: 4,
                modified: None,
            },
            FileEntry {
                path: missing,
                size: 4,
                modified: None,
            },
        ];
        let control = ScanControl::default();
        assert!(
            find_exact_duplicates_controlled(&entries, &HashMap::new(), |_, _| {}, &control)
                .is_empty()
        );
        assert_eq!(control.warnings().len(), 1);
    }

    #[test]
    fn hashing_honours_cancellation() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.file("data", b"content");
        let control = ScanControl::default();
        control.cancelled.store(true, Ordering::SeqCst);
        assert_eq!(
            hash_file_controlled(&path, &control).unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn a_path_replaced_while_open_no_longer_matches_its_stamp() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.file("target", b"original");
        let before = FileStamp::read(&path).unwrap();
        let file = File::open(&path).unwrap();
        assert!(unchanged(&path, &file, &before).unwrap());
        // The open handle still sees the stamped file; only the name moved on.
        let replacement = dir.file("replacement", b"replacement");
        fs::rename(&replacement, &path).unwrap();
        assert!(!unchanged(&path, &file, &before).unwrap());
    }

    fn entry(path: String, size: u64) -> FileEntry {
        FileEntry {
            path,
            size,
            modified: None,
        }
    }

    #[test]
    fn only_shared_sizes_are_read_and_progress_counts_every_candidate() {
        let dir = crate::test_support::TestDir::new();
        let size = SAMPLE_SIZE * 3;
        let mut content = vec![7u8; size];
        let a = dir.file("a", &content);
        let b = dir.file("b", &content);
        content[0] = 8; // same size, different sample: dropped before full hashing
        let c = dir.file("c", &content);
        // A unique size cannot have a duplicate, so this missing file is never opened.
        let lone = dir.0.join("lone").to_str().unwrap().to_string();
        let entries = [
            entry(a.clone(), size as u64),
            entry(b.clone(), size as u64),
            entry(c, size as u64),
            entry(lone, 5),
        ];
        let control = ScanControl::default();
        let progress = std::sync::Mutex::new(Vec::new());
        let groups = find_exact_duplicates_controlled(
            &entries,
            &HashMap::new(),
            |done, total| progress.lock().unwrap().push((done, total)),
            &control,
        );
        assert!(control.warnings().is_empty(), "{:?}", control.warnings());
        assert_eq!(groups.len(), 1);
        let mut paths: Vec<_> = groups[0]
            .files
            .iter()
            .map(|f| f.entry.path.clone())
            .collect();
        paths.sort();
        assert_eq!(paths, [a, b]);
        // The sampled-out file counts as done up front; each full hash adds one.
        let mut progress = progress.into_inner().unwrap();
        progress.sort();
        assert_eq!(progress, [(1, 3), (2, 3), (3, 3)]);
    }

    #[test]
    fn reclaimable_bytes_count_every_copy_but_one() {
        let dir = crate::test_support::TestDir::new();
        let entries: Vec<_> = ["x", "y", "z"]
            .map(|name| entry(dir.file(name, b"seven!!"), 7))
            .to_vec();
        let groups = find_exact_duplicates(&entries, &HashMap::new(), |_, _| {});
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].reclaimable_bytes, 14);
    }

    #[test]
    fn files_shorter_than_two_samples_are_hashed_whole() {
        let dir = crate::test_support::TestDir::new();
        // Shorter than one sample, so sampling would fail to read it.
        let content = vec![3u8; SAMPLE_SIZE * 5 / 8];
        let entries: Vec<_> = ["p", "q"]
            .map(|name| entry(dir.file(name, &content), content.len() as u64))
            .to_vec();
        let control = ScanControl::default();
        let groups =
            find_exact_duplicates_controlled(&entries, &HashMap::new(), |_, _| {}, &control);
        assert!(control.warnings().is_empty(), "{:?}", control.warnings());
        assert_eq!(groups.len(), 1);
        assert_eq!(
            control.hashes.lock().unwrap().len(),
            2,
            "whole-file hashes are cached"
        );
    }

    fn entries_in(root: &PathBuf) -> Vec<FileEntry> {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                FileEntry {
                    path: entry.path().to_string_lossy().into_owned(),
                    size: entry.metadata().unwrap().len(),
                    modified: None,
                }
            })
            .collect()
    }

    fn full_hash_scan(entries: &[FileEntry]) -> usize {
        let mut by_size: HashMap<u64, Vec<&FileEntry>> = HashMap::new();
        for entry in entries {
            by_size.entry(entry.size).or_default().push(entry);
        }
        let hashed: Vec<_> = by_size
            .into_values()
            .filter(|files| files.len() > 1)
            .flatten()
            .collect::<Vec<_>>()
            .par_iter()
            .map(|entry| {
                (
                    hash_file(&entry.path).unwrap().to_hex().to_string(),
                    entry.size,
                )
            })
            .collect();
        let mut by_hash: HashMap<_, Vec<_>> = HashMap::new();
        for (hash, size) in hashed {
            by_hash.entry((size, hash)).or_default().push(());
        }
        by_hash.values().filter(|files| files.len() > 1).count()
    }

    #[test]
    fn sample_matches_still_get_full_verification() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/hash-test");
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let mut original = vec![1; SAMPLE_SIZE * 3];
        original[SAMPLE_SIZE..SAMPLE_SIZE * 2].fill(2);
        let mut different_middle = original.clone();
        different_middle[SAMPLE_SIZE..SAMPLE_SIZE * 2].fill(3);
        fs::write(root.join("original.bin"), &original).unwrap();
        fs::write(root.join("duplicate.bin"), &original).unwrap();
        fs::write(root.join("different.bin"), different_middle).unwrap();

        let groups = find_exact_duplicates(&entries_in(&root), &HashMap::new(), |_, _| {});
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]
        #[test]
        fn groups_match_content_equality_classes(ids in prop::collection::vec(0u8..4, 2..8)) {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/hash-proptest");
            if root.exists() {
                fs::remove_dir_all(&root).unwrap();
            }
            fs::create_dir_all(&root).unwrap();

            for (i, &id) in ids.iter().enumerate() {
                fs::write(root.join(format!("{i}.bin")), vec![id; 64]).unwrap();
            }

            let groups = find_exact_duplicates(&entries_in(&root), &HashMap::new(), |_, _| {});

            let mut counts: HashMap<u8, usize> = HashMap::new();
            for &id in &ids {
                *counts.entry(id).or_default() += 1;
            }
            let expected_groups = counts.values().filter(|&&c| c > 1).count();

            prop_assert_eq!(groups.len(), expected_groups);
            for group in &groups {
                prop_assert!(group.files.len() > 1);
            }

            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    #[ignore = "creates 100,000 small files; run explicitly for scale validation"]
    fn large_library_scan() {
        use crate::{scan_control::ScanControl, scanner, test_support::TestDir};
        let count: usize = std::env::var("DUPLICATE_FINDER_STRESS_FILES")
            .ok()
            .map(|s| s.parse().expect("invalid stress file count"))
            .unwrap_or(100_000);
        assert!(count >= 100 && count.is_multiple_of(100));
        let dir = TestDir::new();
        let setup = Instant::now();
        for bucket in 0..100 {
            fs::create_dir(dir.0.join(bucket.to_string())).unwrap();
        }
        for index in 0..count {
            // Each set of 100 files has identical contents; sets differ.
            dir.file(
                &format!("{}/{}.bin", index % 100, index),
                &(index as u64 / 100).to_le_bytes(),
            );
        }
        let setup_ms = setup.elapsed().as_millis();
        let control = ScanControl::default();
        let start = Instant::now();
        let entries = scanner::walk_folders_controlled(
            &[
                dir.0.to_str().unwrap().into(),
                dir.0.join("0").to_str().unwrap().into(),
            ],
            true,
            0,
            |_, _| {},
            &control,
        )
        .unwrap();
        let walk_ms = start.elapsed().as_millis();
        assert_eq!(entries.len(), count);
        let start = Instant::now();
        let groups =
            find_exact_duplicates_controlled(&entries, &HashMap::new(), |_, _| {}, &control);
        let hash_ms = start.elapsed().as_millis();
        assert_eq!(groups.len(), count / 100);
        assert!(groups.iter().all(|g| g.files.len() == 100));
        assert_eq!(
            groups.iter().map(|g| g.reclaimable_bytes).sum::<u64>(),
            (count as u64 - count as u64 / 100) * 8
        );
        assert!(control.warnings().is_empty());
        println!(
            "SCALE files={count} groups={} setup_ms={setup_ms} walk_ms={walk_ms} hash_ms={hash_ms}",
            groups.len()
        );
    }

    #[test]
    #[ignore]
    fn benchmark_exact_duplicate_scan() {
        const UNIQUE_FILES: u8 = 48;
        const FILE_SIZE: usize = 8 * 1024 * 1024;
        const RUNS: u32 = 3;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/hash-benchmark");
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        for index in 0..UNIQUE_FILES {
            let mut file = fs::File::create(root.join(format!("{index}.bin"))).unwrap();
            let chunk = vec![index; 64 * 1024];
            for _ in 0..FILE_SIZE / chunk.len() {
                file.write_all(&chunk).unwrap();
            }
        }
        fs::copy(root.join("0.bin"), root.join("duplicate-a.bin")).unwrap();
        fs::copy(root.join("0.bin"), root.join("duplicate-b.bin")).unwrap();

        let entries = entries_in(&root);

        full_hash_scan(&entries);
        find_exact_duplicates(&entries, &HashMap::new(), |_, _| {});

        let baseline_start = Instant::now();
        for _ in 0..RUNS {
            assert_eq!(black_box(full_hash_scan(&entries)), 1);
        }
        let baseline = baseline_start.elapsed() / RUNS;

        let optimized_start = Instant::now();
        for _ in 0..RUNS {
            let groups = find_exact_duplicates(&entries, &HashMap::new(), |_, _| {});
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].files.len(), 3);
            black_box(groups);
        }
        let optimized = optimized_start.elapsed() / RUNS;
        println!("mostly distinct — full: {baseline:?}; sampled: {optimized:?}");

        let duplicate_root = root.join("all-duplicates");
        fs::create_dir(&duplicate_root).unwrap();
        for index in 0..entries.len() {
            fs::copy(
                root.join("0.bin"),
                duplicate_root.join(format!("{index}.bin")),
            )
            .unwrap();
        }
        let duplicate_entries = entries_in(&duplicate_root);
        full_hash_scan(&duplicate_entries);
        find_exact_duplicates(&duplicate_entries, &HashMap::new(), |_, _| {});

        let baseline_start = Instant::now();
        for _ in 0..RUNS {
            assert_eq!(black_box(full_hash_scan(&duplicate_entries)), 1);
        }
        let baseline = baseline_start.elapsed() / RUNS;

        let optimized_start = Instant::now();
        for _ in 0..RUNS {
            let groups = find_exact_duplicates(&duplicate_entries, &HashMap::new(), |_, _| {});
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].files.len(), entries.len());
            black_box(groups);
        }
        let optimized = optimized_start.elapsed() / RUNS;
        println!("all duplicates — full: {baseline:?}; sampled: {optimized:?}");

        fs::remove_dir_all(root).unwrap();
    }
}
