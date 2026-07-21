use crate::model::{DuplicateFile, DuplicateGroup, FileEntry, MediaInfo, MediaKind};
use rayon::prelude::*;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

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

#[derive(Deserialize)]
struct ProbeOutput {
    format: ProbeFormat,
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: String,
}

#[derive(Default, Deserialize)]
struct ProbeStream {
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

fn ffprobe() -> Command {
    let command = Command::new("ffprobe");

    #[cfg(windows)]
    {
        let mut command = command;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    }

    #[cfg(not(windows))]
    command
}

/// Checks for ffprobe. Returns false (without panicking) if it's unavailable,
/// so the caller can disable duration-based matching gracefully.
pub fn init() -> bool {
    ffprobe()
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn probe(path: &str, kind: MediaKind) -> Option<MediaInfo> {
    let stream = match kind {
        MediaKind::Video => "v:0",
        MediaKind::Audio => "a:0",
    };
    let output = ffprobe()
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "format=duration:stream=codec_name,width,height",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    parse_probe(&output.stdout, kind)
}

fn parse_probe(json: &[u8], kind: MediaKind) -> Option<MediaInfo> {
    let output: ProbeOutput = serde_json::from_slice(json).ok()?;
    let duration_secs = output.format.duration.parse().ok()?;
    if duration_secs <= 0.0 {
        return None;
    }
    let stream = output.streams.into_iter().next().unwrap_or_default();
    Some(MediaInfo {
        kind,
        duration_secs,
        width: stream.width,
        height: stream.height,
        codec: stream.codec_name,
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
    exclude: &HashSet<&str>,
) -> Vec<DuplicateGroup> {
    let probed: Vec<(&FileEntry, &MediaInfo)> = entries
        .iter()
        .filter(|e| !exclude.contains(e.path.as_str()))
        .filter_map(|e| media_lookup.get(&e.path).map(|info| (e, info)))
        .collect();

    let mut video: Vec<&(&FileEntry, &MediaInfo)> = Vec::new();
    let mut audio: Vec<&(&FileEntry, &MediaInfo)> = Vec::new();
    for item in &probed {
        match item.1.kind {
            MediaKind::Video => video.push(item),
            MediaKind::Audio => audio.push(item),
        }
    }

    video.sort_by(|a, b| a.1.duration_secs.total_cmp(&b.1.duration_secs));
    audio.sort_by(|a, b| a.1.duration_secs.total_cmp(&b.1.duration_secs));

    let mut groups = Vec::new();
    groups.extend(cluster_sorted(&video, tolerance_secs));
    groups.extend(cluster_sorted(&audio, tolerance_secs));

    groups
}

fn cluster_sorted(
    sorted: &[&(&FileEntry, &MediaInfo)],
    tolerance_secs: f64,
) -> Vec<DuplicateGroup> {
    let mut groups = Vec::new();
    let mut cluster: Vec<&(&FileEntry, &MediaInfo)> = Vec::new();

    let flush = |cluster: &mut Vec<&(&FileEntry, &MediaInfo)>, groups: &mut Vec<DuplicateGroup>| {
        if cluster.len() > 1 {
            let max_size = cluster.iter().map(|(e, _)| e.size).max().unwrap_or(0);
            let total_size: u64 = cluster.iter().map(|(e, _)| e.size).sum();

            groups.push(DuplicateGroup {
                files: cluster
                    .iter()
                    .map(|(e, info)| DuplicateFile {
                        entry: (*e).clone(),
                        media: Some((*info).clone()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_output() {
        let info = parse_probe(
            br#"{"streams":[{"codec_name":"h264","width":1920,"height":1080}],"format":{"duration":"12.5"}}"#,
            MediaKind::Video,
        )
        .unwrap();

        assert_eq!(info.duration_secs, 12.5);
        assert_eq!((info.width, info.height), (Some(1920), Some(1080)));
        assert_eq!(info.codec.as_deref(), Some("h264"));
    }

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            size,
            modified: None,
        }
    }

    fn info(kind: MediaKind, duration_secs: f64) -> MediaInfo {
        MediaInfo {
            kind,
            duration_secs,
            width: None,
            height: None,
            codec: None,
        }
    }

    #[test]
    fn a_lone_file_forms_no_group() {
        let entries = vec![entry("a.mp4", 100)];
        let lookup = HashMap::from([("a.mp4".to_string(), info(MediaKind::Video, 10.0))]);

        let groups = cluster_by_duration(&entries, &lookup, 1.0, &HashSet::new());
        assert!(groups.is_empty());
    }

    #[test]
    fn durations_right_at_the_tolerance_boundary_are_grouped() {
        let entries = vec![entry("a.mp4", 100), entry("b.mp4", 200)];
        let lookup = HashMap::from([
            ("a.mp4".to_string(), info(MediaKind::Video, 10.0)),
            ("b.mp4".to_string(), info(MediaKind::Video, 11.0)),
        ]);

        let groups = cluster_by_duration(&entries, &lookup, 1.0, &HashSet::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[0].reclaimable_bytes, 100);
    }

    #[test]
    fn durations_just_outside_the_tolerance_stay_separate() {
        let entries = vec![entry("a.mp4", 100), entry("b.mp4", 200)];
        let lookup = HashMap::from([
            ("a.mp4".to_string(), info(MediaKind::Video, 10.0)),
            ("b.mp4".to_string(), info(MediaKind::Video, 11.01)),
        ]);

        let groups = cluster_by_duration(&entries, &lookup, 1.0, &HashSet::new());
        assert!(groups.is_empty());
    }

    #[test]
    fn audio_and_video_with_matching_durations_are_not_mixed() {
        let entries = vec![entry("a.mp4", 100), entry("b.mp3", 200)];
        let lookup = HashMap::from([
            ("a.mp4".to_string(), info(MediaKind::Video, 10.0)),
            ("b.mp3".to_string(), info(MediaKind::Audio, 10.0)),
        ]);

        let groups = cluster_by_duration(&entries, &lookup, 1.0, &HashSet::new());
        assert!(groups.is_empty());
    }

    #[test]
    fn excluded_paths_are_skipped_even_if_durations_match() {
        let entries = vec![entry("a.mp4", 100), entry("b.mp4", 200)];
        let lookup = HashMap::from([
            ("a.mp4".to_string(), info(MediaKind::Video, 10.0)),
            ("b.mp4".to_string(), info(MediaKind::Video, 10.0)),
        ]);
        let exclude = HashSet::from(["a.mp4"]);

        let groups = cluster_by_duration(&entries, &lookup, 1.0, &exclude);
        assert!(groups.is_empty());
    }
}
