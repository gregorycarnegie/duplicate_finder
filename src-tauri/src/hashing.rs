use crate::model::{DuplicateFile, DuplicateGroup, FileEntry, MediaInfo};
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

pub fn hash_file(path: &str) -> std::io::Result<blake3::Hash> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

fn sample_hash(path: &str, size: u64) -> std::io::Result<(blake3::Hash, bool)> {
    if size <= (SAMPLE_SIZE * 2) as u64 {
        return hash_file(path).map(|hash| (hash, true));
    }

    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut sample = [0; SAMPLE_SIZE];
    file.read_exact(&mut sample)?;
    hasher.update(&sample);
    file.seek(SeekFrom::End(-(SAMPLE_SIZE as i64)))?;
    file.read_exact(&mut sample)?;
    hasher.update(&sample);
    Ok((hasher.finalize(), false))
}

/// Groups files that share an identical byte-for-byte content hash.
/// Only files that share a size with at least one other file are hashed,
/// since a unique size can never be an exact duplicate.
pub fn find_exact_duplicates<F: Fn(u64, u64) + Sync>(
    entries: &[FileEntry],
    media_lookup: &HashMap<String, MediaInfo>,
    on_progress: F,
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
        .filter_map(|e| {
            sample_hash(&e.path, e.size)
                .ok()
                .map(|(hash, complete)| (hash, complete, *e))
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
        let result = hash_file(&entry.path).ok().map(|hash| (hash, *entry));
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
    use std::{fs, hint::black_box, io::Write, path::PathBuf, time::Instant};

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
