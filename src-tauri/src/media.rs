use crate::model::{DuplicateFile, DuplicateGroup, FileEntry, MatchKind, MediaInfo, MediaKind};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

const VIDEO_EXT: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "3gp", "ts", "vob",
];
const AUDIO_EXT: &[&str] = &[
    "mp3", "wav", "flac", "aac", "ogg", "wma", "m4a", "opus", "aiff", "alac",
];

pub fn media_kind_for(path: &str) -> Option<MediaKind> {
    let ext = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_lowercase();
    if VIDEO_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else if AUDIO_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Audio)
    } else {
        None
    }
}

/// Initializes ffmpeg. Returns false (without panicking) if it's unavailable,
/// so the caller can disable duration-based matching gracefully.
pub fn init() -> bool {
    ffmpeg_next::init().is_ok()
}

pub fn probe(path: &str, kind: MediaKind) -> Option<MediaInfo> {
    let ictx = ffmpeg_next::format::input(&path).ok()?;

    let duration = ictx.duration();
    if duration <= 0 {
        return None;
    }
    let duration_secs = duration as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);

    let mut width = None;
    let mut height = None;
    let mut codec = None;

    if let Some(stream) = ictx.streams().best(ffmpeg_next::media::Type::Video) {
        let params = stream.parameters();
        if let Ok(ctx) = ffmpeg_next::codec::context::Context::from_parameters(params) {
            codec = Some(ctx.id().name().to_string());
            if let Ok(video) = ctx.decoder().video() {
                width = Some(video.width());
                height = Some(video.height());
            }
        }
    } else if let Some(stream) = ictx.streams().best(ffmpeg_next::media::Type::Audio) {
        let params = stream.parameters();
        if let Ok(ctx) = ffmpeg_next::codec::context::Context::from_parameters(params) {
            codec = Some(ctx.id().name().to_string());
        }
    }

    Some(MediaInfo {
        kind,
        duration_secs,
        width,
        height,
        codec,
    })
}

/// Probes every media file in `entries` for duration/codec/resolution info.
/// Files ffmpeg can't open (corrupt, unsupported, or not actually media
/// despite the extension) are silently omitted from the result.
pub fn probe_all<F: Fn(u64, u64) + Sync>(
    entries: &[FileEntry],
    on_progress: F,
) -> HashMap<String, MediaInfo> {
    let candidates: Vec<&FileEntry> = entries
        .iter()
        .filter(|e| media_kind_for(&e.path).is_some())
        .collect();

    let total = candidates.len() as u64;
    let done = AtomicU64::new(0);

    candidates
        .par_iter()
        .filter_map(|e| {
            let kind = media_kind_for(&e.path)?;
            let result = probe(&e.path, kind).map(|info| (e.path.clone(), info));
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            on_progress(d, total);
            result
        })
        .collect()
}

/// Clusters previously-probed media files whose durations fall within
/// `tolerance_secs` of each other into "likely duplicate" groups. Files
/// already proven byte-identical (in `exclude`) are skipped, since they're
/// already reported as exact duplicates.
pub fn cluster_by_duration(
    entries: &[FileEntry],
    media_lookup: &HashMap<String, MediaInfo>,
    tolerance_secs: f64,
    exclude: &HashSet<String>,
) -> Vec<DuplicateGroup> {
    let probed: Vec<(&FileEntry, MediaInfo)> = entries
        .iter()
        .filter(|e| !exclude.contains(&e.path))
        .filter_map(|e| media_lookup.get(&e.path).map(|info| (e, info.clone())))
        .collect();

    let mut video: Vec<&(&FileEntry, MediaInfo)> = Vec::new();
    let mut audio: Vec<&(&FileEntry, MediaInfo)> = Vec::new();
    for item in &probed {
        match item.1.kind {
            MediaKind::Video => video.push(item),
            MediaKind::Audio => audio.push(item),
        }
    }

    video.sort_by(|a, b| a.1.duration_secs.total_cmp(&b.1.duration_secs));
    audio.sort_by(|a, b| a.1.duration_secs.total_cmp(&b.1.duration_secs));

    let mut groups = Vec::new();
    groups.extend(cluster_sorted(&video, tolerance_secs, "video"));
    groups.extend(cluster_sorted(&audio, tolerance_secs, "audio"));

    groups
}

fn cluster_sorted(
    sorted: &[&(&FileEntry, MediaInfo)],
    tolerance_secs: f64,
    label: &str,
) -> Vec<DuplicateGroup> {
    let mut groups = Vec::new();
    let mut cluster: Vec<&(&FileEntry, MediaInfo)> = Vec::new();

    let flush = |cluster: &mut Vec<&(&FileEntry, MediaInfo)>, groups: &mut Vec<DuplicateGroup>| {
        if cluster.len() > 1 {
            let min_d = cluster
                .iter()
                .map(|(_, i)| i.duration_secs)
                .fold(f64::MAX, f64::min);
            let max_d = cluster
                .iter()
                .map(|(_, i)| i.duration_secs)
                .fold(f64::MIN, f64::max);
            let max_size = cluster.iter().map(|(e, _)| e.size).max().unwrap_or(0);
            let total_size: u64 = cluster.iter().map(|(e, _)| e.size).sum();

            groups.push(DuplicateGroup {
                id: format!("media-{}-{}", label, groups.len()),
                match_kind: MatchKind::MediaDuration {
                    spread_secs: max_d - min_d,
                },
                files: cluster
                    .iter()
                    .map(|(e, info)| DuplicateFile {
                        entry: (*e).clone(),
                        media: Some(info.clone()),
                    })
                    .collect(),
                reclaimable_bytes: total_size.saturating_sub(max_size),
            });
        }
        cluster.clear();
    };

    for item in sorted {
        if let Some(last) = cluster.last() {
            if item.1.duration_secs - last.1.duration_secs > tolerance_secs {
                flush(&mut cluster, &mut groups);
            }
        }
        cluster.push(item);
    }
    flush(&mut cluster, &mut groups);

    groups
}
