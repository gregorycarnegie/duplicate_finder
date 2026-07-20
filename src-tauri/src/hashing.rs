use crate::model::{DuplicateFile, DuplicateGroup, FileEntry, MediaInfo};
use rayon::prelude::*;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read},
    sync::atomic::{AtomicU64, Ordering},
};

const CHUNK_SIZE: usize = 1024 * 1024;

pub fn hash_file(path: &str) -> std::io::Result<String> {
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
    Ok(hasher.finalize().to_hex().to_string())
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
    let done = AtomicU64::new(0);

    let hashed: Vec<(String, &FileEntry)> = candidates
        .par_iter()
        .filter_map(|e| {
            let result = hash_file(&e.path).ok().map(|h| (h, *e));
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            on_progress(d, total);
            result
        })
        .collect();

    let mut by_hash: HashMap<(u64, String), Vec<&FileEntry>> = HashMap::new();
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
