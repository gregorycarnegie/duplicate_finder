use crate::model::{DuplicateFile, DuplicateGroup, FileEntry, MediaInfo, MediaKind};
use crate::scan_control::ScanControl;
use rayon::prelude::*;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
use std::{
    io::{self, Read},
    thread,
    time::{Duration, Instant},
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

pub(crate) fn media_command(program: &str) -> Command {
    let command = Command::new(program);

    #[cfg(windows)]
    {
        let mut command = command;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    }

    #[cfg(not(windows))]
    command
}

// Read bounded output concurrently so a full pipe cannot stall the child.
pub(crate) fn run_command(
    command: &mut Command,
    control: &ScanControl,
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    control.check()?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut output)
            .map(|_| output)
    });
    let start = Instant::now();
    let status = loop {
        if let Err(error) = control.check() {
            break Err(error);
        }
        if start.elapsed() >= timeout {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Media process timed out",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => break Err(error),
        }
    };
    if status.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let output = reader
        .join()
        .map_err(|_| io::Error::other("Cannot read media process output"))??;
    if !status?.success() {
        return Err(io::Error::other(
            "Media process failed (unsupported codec, missing feature, or unreadable input)",
        ));
    }
    if output.len() > 1024 * 1024 {
        return Err(io::Error::other("Media process output exceeded limit"));
    }
    Ok(output)
}

pub fn init(control: &ScanControl) -> bool {
    run_command(
        media_command("ffprobe").arg("-version"),
        control,
        Duration::from_secs(5),
    )
    .is_ok()
}

pub fn probe(path: &str, kind: MediaKind, control: &ScanControl) -> io::Result<MediaInfo> {
    let stream = match kind {
        MediaKind::Video => "v:0",
        MediaKind::Audio => "a:0",
    };
    let output = run_command(
        media_command("ffprobe")
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
            .arg(path),
        control,
        Duration::from_secs(30),
    )?;
    parse_probe(&output, kind)
        .ok_or_else(|| io::Error::other("Media duration is missing or invalid"))
}

fn parse_probe(json: &[u8], kind: MediaKind) -> Option<MediaInfo> {
    let output: ProbeOutput = serde_json::from_slice(json).ok()?;
    let duration_secs: f64 = output.format.duration.parse().ok()?;
    if !duration_secs.is_finite() || duration_secs <= 0.0 {
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
/// Failed probes are recorded as scan warnings.
pub fn probe_all<F: Fn(u64, u64) + Sync>(
    entries: &[FileEntry],
    on_progress: F,
    control: &ScanControl,
) -> HashMap<String, MediaInfo> {
    let candidates: Vec<&FileEntry> = entries
        .iter()
        .filter(|e| media_kind_for(&e.path).is_some())
        .collect();

    let total = candidates.len() as u64;
    let done = AtomicU64::new(0);

    let pool = match rayon::ThreadPoolBuilder::new().num_threads(4).build() {
        Ok(pool) => pool,
        Err(error) => {
            control.warn("Media probing", error);
            return HashMap::new();
        }
    };
    pool.install(|| {
        candidates
            .par_iter()
            .filter_map(|e| {
                let kind = media_kind_for(&e.path)?;
                let result = match probe(&e.path, kind, control) {
                    Ok(info) => Some((e.path.clone(), info)),
                    Err(error) => {
                        control.warn(&e.path, error);
                        None
                    }
                };
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(d, total);
                result
            })
            .collect()
    })
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
                evidence: Default::default(),
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
        if let Some(first) = cluster.first()
            && item.1.duration_secs - first.1.duration_secs > tolerance_secs
        {
            flush(&mut cluster, &mut groups);
        }
        cluster.push(item);
    }
    flush(&mut cluster, &mut groups);

    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Captured from ffprobe 8.1.2 with probe()'s arguments, for an H.264 MP4
    // and an MP3. Note the extra keys, padded durations and absent dimensions.
    const REAL_VIDEO_PROBE: &str = r#"{
    "programs": [

    ],
    "stream_groups": [

    ],
    "streams": [
        {
            "codec_name": "h264",
            "width": 320,
            "height": 240
        }
    ],
    "format": {
        "duration": "3.000000"
    }
}"#;
    const REAL_AUDIO_PROBE: &str = r#"{
    "programs": [

    ],
    "stream_groups": [

    ],
    "streams": [
        {
            "codec_name": "mp3"
        }
    ],
    "format": {
        "duration": "2.500000"
    }
}"#;

    #[test]
    fn parses_real_ffprobe_output() {
        let video = parse_probe(REAL_VIDEO_PROBE.as_bytes(), MediaKind::Video).unwrap();
        assert!(matches!(video.kind, MediaKind::Video));
        assert_eq!(video.duration_secs, 3.0);
        assert_eq!((video.width, video.height), (Some(320), Some(240)));
        assert_eq!(video.codec.as_deref(), Some("h264"));
        let audio = parse_probe(REAL_AUDIO_PROBE.as_bytes(), MediaKind::Audio).unwrap();
        assert!(matches!(audio.kind, MediaKind::Audio));
        assert_eq!(audio.duration_secs, 2.5);
        assert_eq!((audio.width, audio.height), (None, None));
        assert_eq!(audio.codec.as_deref(), Some("mp3"));
    }

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

    #[test]
    fn duration_chains_cannot_exceed_the_tolerance() {
        let entries = vec![entry("a", 1), entry("b", 2), entry("c", 3)];
        let lookup = HashMap::from([
            ("a".into(), info(MediaKind::Video, 10.0)),
            ("b".into(), info(MediaKind::Video, 10.8)),
            ("c".into(), info(MediaKind::Video, 11.6)),
        ]);
        let groups = cluster_by_duration(&entries, &lookup, 1.0, &HashSet::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
    }

    #[test]
    fn non_finite_durations_are_rejected() {
        for duration in ["NaN", "inf", "-inf", "0", "-1"] {
            let json = format!(r#"{{"format":{{"duration":"{duration}"}},"streams":[]}}"#);
            assert!(parse_probe(json.as_bytes(), MediaKind::Video).is_none());
        }
    }

    /// A shell command: `unix` under sh, `windows` under PowerShell.
    fn shell(unix: &str, windows: &str) -> Command {
        let mut command;
        if cfg!(windows) {
            command = Command::new("powershell");
            command.args(["-NoProfile", "-NonInteractive", "-Command", windows]);
        } else {
            command = Command::new("sh");
            command.args(["-c", unix]);
        }
        command
    }

    fn output_of(bytes: usize) -> Command {
        shell(
            &format!("head -c {bytes} /dev/zero"),
            &format!(
                "$o = [Console]::OpenStandardOutput(); $o.Write((New-Object byte[] {bytes}), 0, {bytes})"
            ),
        )
    }

    fn sleeper() -> Command {
        shell("exec sleep 10", "Start-Sleep 10")
    }

    #[test]
    fn stalled_process_is_killed_on_timeout() {
        let start = Instant::now();
        let error = run_command(
            &mut sleeper(),
            &ScanControl::default(),
            Duration::from_millis(60),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn cancellation_kills_a_running_process() {
        let control = ScanControl::default();
        let cancel = control.cancelled.clone();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(70));
            cancel.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let error = run_command(&mut sleeper(), &control, Duration::from_secs(30)).unwrap_err();
        thread.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_cancelled_scan_starts_no_process() {
        let control = ScanControl::default();
        control.cancelled.store(true, Ordering::SeqCst);
        assert!(!init(&control), "FFmpeg reported available without a check");
        let error = run_command(
            &mut Command::new("does-not-exist"),
            &control,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn child_output_larger_than_a_pipe_does_not_deadlock() {
        let result = run_command(
            &mut output_of(100_000),
            &ScanControl::default(),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(result.len(), 100_000);
    }

    #[test]
    fn output_is_capped_at_one_mebibyte() {
        let limit = 1024 * 1024;
        let control = ScanControl::default();
        let at_limit =
            run_command(&mut output_of(limit), &control, Duration::from_secs(30)).unwrap();
        assert_eq!(at_limit.len(), limit);
        let error =
            run_command(&mut output_of(limit + 1), &control, Duration::from_secs(30)).unwrap_err();
        assert_eq!(error.to_string(), "Media process output exceeded limit");
    }

    #[test]
    fn a_failing_process_is_an_error_even_with_output() {
        let error = run_command(
            &mut shell("echo partial; exit 3", "Write-Output partial; exit 3"),
            &ScanControl::default(),
            Duration::from_secs(30),
        )
        .unwrap_err();
        assert!(
            error.to_string().starts_with("Media process failed"),
            "{error}"
        );
    }

    #[test]
    fn probing_skips_other_files_and_reports_each_failed_probe() {
        let entries = [entry("notes.txt", 1), entry("missing/clip.mp4", 1)];
        let control = ScanControl::default();
        let progress = std::sync::Mutex::new(Vec::new());
        let found = probe_all(
            &entries,
            |done, total| progress.lock().unwrap().push((done, total)),
            &control,
        );
        assert!(found.is_empty());
        assert_eq!(progress.into_inner().unwrap(), [(1, 1)]);
        let warnings = control.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("missing/clip.mp4: "),
            "{warnings:?}"
        );
    }

    #[test]
    fn media_kind_comes_from_the_extension_in_any_case() {
        assert!(matches!(media_kind_for("a/b.MKV"), Some(MediaKind::Video)));
        assert!(matches!(
            media_kind_for("song.Flac"),
            Some(MediaKind::Audio)
        ));
        assert!(media_kind_for("mp4").is_none());
        assert!(media_kind_for("notes.txt").is_none());
    }

    #[test]
    #[ignore = "requires ffmpeg"]
    fn real_probe_reads_the_stream_for_the_requested_kind() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.0.join("both.mkv").to_str().unwrap().to_string();
        let control = ScanControl::default();
        run_command(
            media_command("ffmpeg").args([
                "-nostdin",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=96x64:rate=5:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "ffv1",
                "-c:a",
                "flac",
                &path,
            ]),
            &control,
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(init(&control));
        let video = probe(&path, MediaKind::Video, &control).unwrap();
        assert_eq!(
            (video.codec.as_deref(), video.width, video.height),
            (Some("ffv1"), Some(96), Some(64))
        );
        let audio = probe(&path, MediaKind::Audio, &control).unwrap();
        assert_eq!((audio.codec.as_deref(), audio.width), (Some("flac"), None));
        assert!((audio.duration_secs - 2.0).abs() < 0.1);
    }

    proptest! {
        #[test]
        fn probe_parsing_never_panics_on_arbitrary_output(
            bytes in prop::collection::vec(any::<u8>(), 0..200),
            duration in ".{0,12}",
        ) {
            let _ = parse_probe(&bytes, MediaKind::Video);
            let json = serde_json::json!({ "format": { "duration": duration }, "streams": [] });
            if let Some(info) = parse_probe(json.to_string().as_bytes(), MediaKind::Audio) {
                prop_assert!(info.duration_secs.is_finite() && info.duration_secs > 0.0);
            }
        }

        #[test]
        fn clusters_stay_within_tolerance_and_never_reuse_a_file(
            durations in prop::collection::vec(0.0f64..1000.0, 0..12),
            tolerance in 0.0f64..5.0,
        ) {
            let entries: Vec<FileEntry> = durations
                .iter()
                .enumerate()
                .map(|(i, _)| entry(&format!("f{i}.mp4"), (i as u64 + 1) * 100))
                .collect();
            let lookup: HashMap<String, MediaInfo> = entries
                .iter()
                .zip(&durations)
                .map(|(e, &d)| (e.path.clone(), info(MediaKind::Video, d)))
                .collect();

            let groups = cluster_by_duration(&entries, &lookup, tolerance, &HashSet::new());

            let mut seen = HashSet::new();
            for group in &groups {
                prop_assert!(group.files.len() > 1);

                let mut group_durations: Vec<f64> = group
                    .files
                    .iter()
                    .map(|f| lookup[&f.entry.path].duration_secs)
                    .collect();
                group_durations.sort_by(f64::total_cmp);
                prop_assert!(group_durations.last().unwrap() - group_durations[0] <= tolerance + 1e-9);

                let total: u64 = group.files.iter().map(|f| f.entry.size).sum();
                let max = group.files.iter().map(|f| f.entry.size).max().unwrap();
                prop_assert_eq!(group.reclaimable_bytes, total - max);

                for f in &group.files {
                    prop_assert!(seen.insert(f.entry.path.clone()), "path grouped twice");
                }
            }
        }
    }
}
