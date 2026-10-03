//! Optional, conservative media comparisons. These are review suggestions, never
//! proof of complete equivalence: frames omit audio; audio samples omit most of a
//! long recording. No fingerprints or file contents leave the machine.
use crate::{
    media::{media_command, run_command},
    model::{DuplicateFile, DuplicateGroup, MatchEvidence, MediaKind},
    scan_control::ScanControl,
};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    io,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[derive(Clone, Debug)]
enum Fingerprint {
    Video(Vec<u64>),
    Audio(Vec<Vec<u32>>),
}

fn frame_hash(pixels: &[u8]) -> Option<u64> {
    if pixels.len() != 72 {
        return None;
    }
    let contrast = pixels.iter().max()? - pixels.iter().min()?;
    if contrast < 20 {
        return None;
    }
    let mut hash = 0u64;
    for (index, row) in pixels.as_chunks::<9>().0.iter().enumerate() {
        for col in 0..8 {
            if row[col] > row[col + 1] {
                hash |= 1 << (index * 8 + col);
            }
        }
    }
    (8..=56).contains(&hash.count_ones()).then_some(hash)
}

/// FFmpeg's raw Chromaprint muxer writes native-endian uint32s.
fn chromaprint_hashes(raw: &[u8]) -> Option<Vec<u32>> {
    let (words, partial) = raw.as_chunks::<4>();
    let hashes: Vec<u32> = words.iter().map(|b| u32::from_ne_bytes(*b)).collect();
    (partial.is_empty() && hashes.len() >= 16 && hashes.iter().collect::<HashSet<_>>().len() >= 4)
        .then_some(hashes)
}

/// Where a sample starts: `fraction` through a video, or through the part of
/// a recording that still leaves a whole 15-second audio window.
fn sample_time(kind: MediaKind, duration_secs: f64, fraction: f64) -> f64 {
    match kind {
        MediaKind::Video => duration_secs * fraction,
        MediaKind::Audio => (duration_secs - 15.0).max(0.0) * fraction,
    }
}

fn extract(file: &DuplicateFile, control: &ScanControl) -> io::Result<Fingerprint> {
    let info = file
        .media
        .as_ref()
        .ok_or_else(|| io::Error::other("Missing media metadata"))?;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for fraction in [0.2, 0.5, 0.8] {
        control.check()?;
        let time = sample_time(info.kind, info.duration_secs, fraction);
        let mut command = media_command("ffmpeg");
        command
            .args([
                "-nostdin",
                "-v",
                "error",
                "-threads",
                "1",
                "-ss",
                &format!("{time:.3}"),
                "-i",
            ])
            .arg(&file.entry.path);
        match info.kind {
            MediaKind::Video => {
                command.args([
                    "-map",
                    "0:v:0",
                    "-an",
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=9:8:flags=area,format=gray",
                    "-threads",
                    "1",
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ]);
                let pixels = run_command(&mut command, control, Duration::from_secs(30))?;
                video.push(frame_hash(&pixels).ok_or_else(|| {
                    io::Error::other(
                        "Video sample has too little detail for a reliable fingerprint",
                    )
                })?);
            }
            MediaKind::Audio => {
                command.args([
                    "-map",
                    "0:a:0",
                    "-vn",
                    "-t",
                    "15",
                    "-ac",
                    "1",
                    "-ar",
                    "11025",
                    "-f",
                    "chromaprint",
                    "-fp_format",
                    "raw",
                    "pipe:1",
                ]);
                let raw = run_command(&mut command, control, Duration::from_secs(30))?;
                audio.push(chromaprint_hashes(&raw).ok_or_else(|| {
                    io::Error::other(
                        "Audio sample is too short or repetitive for a reliable fingerprint",
                    )
                })?);
            }
        }
    }
    Ok(match info.kind {
        MediaKind::Video => Fingerprint::Video(video),
        MediaKind::Audio => Fingerprint::Audio(audio),
    })
}

fn audio_matches(a: &[u32], b: &[u32]) -> bool {
    if a.len() < 16 || b.len() < 16 {
        return false;
    }
    // Permit a small codec-delay offset, requiring >=80% overlap and <=10%
    // differing fingerprint bits. Thresholds are conservative heuristics.
    (-3i32..=3).any(|offset| {
        let a = &a[offset.max(0) as usize..];
        let b = &b[(-offset).max(0) as usize..];
        let n = a.len().min(b.len());
        n >= 16
            && n * 5 >= a.len().max(b.len()) * 4
            && a.iter()
                .zip(b)
                .map(|(a, b)| (a ^ b).count_ones() as usize)
                .sum::<usize>()
                * 10
                <= n * 32
    })
}

fn similar(a: &Fingerprint, b: &Fingerprint) -> bool {
    match (a, b) {
        (Fingerprint::Video(a), Fingerprint::Video(b)) => {
            a.len() == 3 && b.len() == 3 && a.iter().zip(b).all(|(a, b)| (a ^ b).count_ones() <= 8)
        }
        (Fingerprint::Audio(a), Fingerprint::Audio(b)) => {
            a.len() == 3 && b.len() == 3 && a.iter().zip(b).all(|(a, b)| audio_matches(a, b))
        }
        _ => false,
    }
}

pub fn refine_groups<F: Fn(u64, u64) + Sync>(
    groups: Vec<DuplicateGroup>,
    control: &ScanControl,
    progress: F,
) -> io::Result<Vec<DuplicateGroup>> {
    if groups.is_empty() {
        return Ok(groups);
    }
    if let Err(error) = run_command(
        media_command("ffmpeg").arg("-version"),
        control,
        Duration::from_secs(5),
    ) {
        control.check()?;
        control.warn(
            "Content comparison unavailable; showing duration-only groups",
            error,
        );
        return Ok(groups);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .map_err(io::Error::other)?;
    let total = groups.iter().map(|g| g.files.len() as u64).sum();
    let done = AtomicU64::new(0);
    progress(0, total);
    let mut result = Vec::new();
    for group in groups {
        control.check()?;
        let fingerprints: Vec<_> = pool.install(|| {
            group
                .files
                .par_iter()
                .map(|file| {
                    let value = extract(file, control);
                    if let Err(error) = &value {
                        control.warn(
                            &file.entry.path,
                            format!("Content comparison unavailable: {error}"),
                        );
                    }
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                    value.ok()
                })
                .collect()
        });
        control.check()?;
        result.extend(partition(group.files, fingerprints, control)?);
    }
    Ok(result)
}

/// Splits one duration group by fingerprint similarity. Files without a
/// fingerprint stay together as a duration-only group.
fn partition(
    files: Vec<DuplicateFile>,
    fingerprints: Vec<Option<Fingerprint>>,
    control: &ScanControl,
) -> io::Result<Vec<DuplicateGroup>> {
    let mut result = Vec::new();
    let mut partitions: Vec<(Fingerprint, Vec<DuplicateFile>)> = Vec::new();
    let mut unavailable = Vec::new();
    for (file, fingerprint) in files.into_iter().zip(fingerprints) {
        control.check()?;
        let Some(fingerprint) = fingerprint else {
            unavailable.push(file);
            continue;
        };
        if let Some((_, files)) = partitions
            .iter_mut()
            .find(|(reference, _)| similar(reference, &fingerprint))
        {
            files.push(file);
        } else if partitions.len() < 256 {
            partitions.push((fingerprint, vec![file]));
        } else {
            // Bound worst-case comparisons for enormous same-duration buckets.
            control.warn(
                &file.entry.path,
                "Too many distinct fingerprints in this duration group; content comparison skipped",
            );
            unavailable.push(file);
        }
    }
    for (fingerprint, files) in partitions {
        if files.len() > 1 {
            result.push(DuplicateGroup {
                files,
                reclaimable_bytes: 0,
                evidence: match fingerprint {
                    Fingerprint::Video(_) => MatchEvidence::VideoFrames,
                    Fingerprint::Audio(_) => MatchEvidence::AudioFingerprint,
                },
            });
        }
    }
    if unavailable.len() > 1 {
        result.push(DuplicateGroup {
            files: unavailable,
            reclaimable_bytes: 0,
            evidence: MatchEvidence::Duration,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{media, model::FileEntry, test_support::TestDir};
    fn fixture_file(path: String, kind: MediaKind, control: &ScanControl) -> DuplicateFile {
        let info = media::probe(&path, kind, control).unwrap();
        DuplicateFile {
            entry: FileEntry {
                size: std::fs::metadata(&path).unwrap().len(),
                path,
                modified: None,
            },
            media: Some(info),
        }
    }
    fn encode(args: &[&str], control: &ScanControl) {
        run_command(
            media_command("ffmpeg")
                .args(["-nostdin", "-v", "error", "-y"])
                .args(args),
            control,
            Duration::from_secs(30),
        )
        .unwrap();
    }
    #[test]
    #[ignore = "requires ffmpeg and real media encoding"]
    fn real_video_reencoding_matches_but_mirrored_content_does_not() {
        let dir = TestDir::new();
        let a = dir.0.join("original.mkv").to_str().unwrap().to_string();
        let b = dir.0.join("resized.mp4").to_str().unwrap().to_string();
        let c = dir.0.join("different.mp4").to_str().unwrap().to_string();
        let control = ScanControl::default();
        encode(
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=10:duration=12",
                "-c:v",
                "ffv1",
                &a,
            ],
            &control,
        );
        encode(
            &[
                "-i",
                &a,
                "-vf",
                "scale=80:60",
                "-c:v",
                "mpeg4",
                "-q:v",
                "6",
                &b,
            ],
            &control,
        );
        encode(
            &["-i", &a, "-vf", "hflip", "-c:v", "mpeg4", "-q:v", "6", &c],
            &control,
        );
        let files = [a, b, c]
            .into_iter()
            .map(|p| fixture_file(p, MediaKind::Video, &control))
            .collect();
        let groups = refine_groups(
            vec![DuplicateGroup {
                files,
                evidence: MatchEvidence::Duration,
                reclaimable_bytes: 0,
            }],
            &control,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(groups.len(), 1, "warnings: {:?}", control.warnings());
        assert_eq!(groups[0].files.len(), 2);
        assert!(matches!(groups[0].evidence, MatchEvidence::VideoFrames));
        assert!(control.warnings().is_empty());
    }
    fn write_music(path: &str, notes: &[f64]) {
        use std::io::Write;
        let rate = 22050u32;
        let samples = rate * 24;
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + samples * 2).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&rate.to_le_bytes()).unwrap();
        file.write_all(&(rate * 2).to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(samples * 2).to_le_bytes()).unwrap();
        let mut data = Vec::with_capacity(samples as usize * 2);
        for i in 0..samples {
            let frequency = notes[((i / (rate / 3)) as usize) % notes.len()];
            let phase = std::f64::consts::TAU * frequency * i as f64 / rate as f64;
            let sample = (6000.0
                * (phase.sin() + 0.5 * (phase * 1.5).sin() + 0.3 * (phase * 2.0).sin()))
                as i16;
            data.extend(sample.to_le_bytes());
        }
        file.write_all(&data).unwrap();
    }
    #[test]
    #[ignore = "requires ffmpeg with Chromaprint and MP3 encoding"]
    fn real_audio_reencoding_matches_but_different_music_does_not() {
        let dir = TestDir::new();
        let a = dir.0.join("original.wav").to_str().unwrap().to_string();
        let b = dir.0.join("encoded.mp3").to_str().unwrap().to_string();
        let c = dir.0.join("different.wav").to_str().unwrap().to_string();
        write_music(&a, &[220., 277., 330., 440., 294., 392., 247., 349.]);
        write_music(
            &c,
            &[
                195., 310., 233., 170., 415., 185., 261., 370., 466., 311., 207.,
            ],
        );
        let control = ScanControl::default();
        encode(
            &["-i", &a, "-c:a", "libmp3lame", "-b:a", "96k", &b],
            &control,
        );
        let files = [a, b, c]
            .into_iter()
            .map(|p| fixture_file(p, MediaKind::Audio, &control))
            .collect();
        let groups = refine_groups(
            vec![DuplicateGroup {
                files,
                evidence: MatchEvidence::Duration,
                reclaimable_bytes: 0,
            }],
            &control,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(groups.len(), 1, "warnings: {:?}", control.warnings());
        assert_eq!(groups[0].files.len(), 2);
        assert!(matches!(
            groups[0].evidence,
            MatchEvidence::AudioFingerprint
        ));
        assert!(control.warnings().is_empty());
    }

    #[test]
    fn uniform_frames_and_empty_audio_cannot_be_matches() {
        assert!(frame_hash(&[0; 72]).is_none());
        assert!(frame_hash(&[255; 72]).is_none());
        assert!(!similar(
            &Fingerprint::Audio(vec![]),
            &Fingerprint::Audio(vec![])
        ));
        assert!(!similar(
            &Fingerprint::Video(vec![]),
            &Fingerprint::Video(vec![])
        ));
    }
    #[test]
    fn every_video_sample_must_match() {
        let a = Fingerprint::Video(vec![0xaaaaaaaaaaaaaaaa; 3]);
        assert!(similar(
            &a,
            &Fingerprint::Video(vec![0xaaaaaaaaaaaaaaab; 3])
        ));
        assert!(!similar(
            &a,
            &Fingerprint::Video(vec![
                0xaaaaaaaaaaaaaaaa,
                0xaaaaaaaaaaaaaaaa,
                0x5555555555555555
            ])
        ));
    }

    #[test]
    fn samples_spread_through_video_and_leave_audio_a_full_window() {
        assert_eq!(sample_time(MediaKind::Video, 40.0, 0.5), 20.0);
        assert_eq!(sample_time(MediaKind::Audio, 40.0, 0.5), 12.5);
        assert_eq!(sample_time(MediaKind::Audio, 10.0, 0.5), 0.0);
    }

    /// Eight 9-pixel rows; the first `bits` horizontal steps, row-major,
    /// get brighter-to-darker edges and every other step is flat.
    fn frame_with_bits(mut bits: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(72);
        for _ in 0..8 {
            let edges = bits.min(8);
            bits -= edges;
            let mut value = 100u8;
            pixels.push(value);
            for col in 0..8 {
                if col < edges {
                    value -= 5;
                }
                pixels.push(value);
            }
        }
        pixels
    }

    #[test]
    fn frame_hash_sets_one_bit_per_darkening_step_row_major() {
        assert_eq!(frame_hash(&frame_with_bits(12)), Some(0x0fff));
        assert_eq!(frame_hash(&frame_with_bits(8)), Some(0xff));
        assert_eq!(frame_hash(&frame_with_bits(56)), Some((1 << 56) - 1));
        // Too few or too many edges is a flat or saturated frame, not detail.
        assert_eq!(frame_hash(&frame_with_bits(7)), None);
        assert_eq!(frame_hash(&frame_with_bits(57)), None);
    }

    #[test]
    fn frame_hash_needs_72_pixels_and_twenty_levels_of_contrast() {
        let striped = |low: u8| -> Vec<u8> {
            (0..72)
                .map(|i| if i % 2 == 0 { 120 } else { low })
                .collect()
        };
        // Rows have 9 pixels, so the stripe phase alternates row to row.
        assert_eq!(frame_hash(&striped(100)), Some(0xaa55_aa55_aa55_aa55));
        assert_eq!(frame_hash(&striped(101)), None);
        let mut short = frame_with_bits(32);
        short.pop();
        assert_eq!(frame_hash(&short), None);
        let mut long = frame_with_bits(32);
        long.push(100);
        assert_eq!(frame_hash(&long), None);
    }

    /// Deterministic noise: neighbouring words share no structure, so any
    /// misalignment differs in about half its bits.
    fn noise(len: usize) -> Vec<u32> {
        let mut x = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x
            })
            .collect()
    }

    /// Flips `bits` bits spread across the words, never twice in one place.
    fn flip(words: &[u32], bits: usize) -> Vec<u32> {
        let n = words.len();
        words
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let k = bits / n + usize::from(i < bits % n);
                w ^ ((1u64 << k) - 1) as u32
            })
            .collect()
    }

    #[test]
    fn audio_matches_tolerates_three_words_of_codec_delay_either_way() {
        let a = noise(40);
        assert!(audio_matches(&a, &a));
        assert!(audio_matches(&a, &a[3..]));
        assert!(audio_matches(&a[3..], &a));
        assert!(!audio_matches(&a, &a[4..]));
        assert!(!audio_matches(&a[4..], &a));
    }

    #[test]
    fn audio_matches_requires_sixteen_words_and_eighty_percent_overlap() {
        let a = noise(21);
        assert!(audio_matches(&a[..16], &a[..16]));
        assert!(!audio_matches(&a[..15], &a[..15]));
        // Too short to shift by the codec-delay window, and must not panic.
        assert!(!audio_matches(&a[..2], &a));
        assert!(audio_matches(&a[..20], &a[..16]));
        assert!(!audio_matches(&a, &a[..16]));
    }

    #[test]
    fn audio_matches_allows_at_most_a_tenth_of_bits_to_differ() {
        let a = noise(20);
        assert!(audio_matches(&a, &flip(&a, 64)));
        assert!(!audio_matches(&a, &flip(&a, 65)));
    }

    #[test]
    fn similar_compares_exactly_three_samples_of_the_same_kind() {
        let x = 0x0123_4567_89ab_cdefu64;
        let video = Fingerprint::Video(vec![x; 3]);
        assert!(similar(&video, &Fingerprint::Video(vec![x ^ 0xff; 3])));
        assert!(!similar(&video, &Fingerprint::Video(vec![x ^ 0x1ff; 3])));
        assert!(!similar(&video, &Fingerprint::Video(vec![x; 4])));
        assert!(!similar(&Fingerprint::Video(vec![x; 4]), &video));
        let words = noise(20);
        let audio = Fingerprint::Audio(vec![words.clone(); 3]);
        assert!(similar(&audio, &audio));
        assert!(!similar(
            &audio,
            &Fingerprint::Audio(vec![words.clone(); 4])
        ));
        assert!(!similar(
            &Fingerprint::Audio(vec![words.clone(); 4]),
            &audio
        ));
        assert!(!similar(
            &audio,
            &Fingerprint::Audio(vec![words.clone(), words.clone(), noise(40)[20..].to_vec()])
        ));
        assert!(!similar(&video, &audio));
        assert!(!similar(&audio, &video));
    }

    fn chromaprint_bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_ne_bytes()).collect()
    }

    #[test]
    fn chromaprint_output_needs_sixteen_whole_words_with_four_distinct() {
        let words = noise(16);
        assert_eq!(
            chromaprint_hashes(&chromaprint_bytes(&words)),
            Some(words.clone())
        );
        assert_eq!(chromaprint_hashes(&chromaprint_bytes(&words[..15])), None);
        let mut ragged = chromaprint_bytes(&words);
        ragged.push(0);
        assert_eq!(chromaprint_hashes(&ragged), None);
        let four: Vec<u32> = (0..16).map(|i| words[i % 4]).collect();
        assert_eq!(chromaprint_hashes(&chromaprint_bytes(&four)), Some(four));
        let three: Vec<u32> = (0..16).map(|i| words[i % 3]).collect();
        assert_eq!(chromaprint_hashes(&chromaprint_bytes(&three)), None);
    }

    fn file(path: &str) -> DuplicateFile {
        DuplicateFile {
            entry: FileEntry {
                path: path.into(),
                size: 1,
                modified: None,
            },
            media: None,
        }
    }

    fn paths(group: &DuplicateGroup) -> Vec<&str> {
        group.files.iter().map(|f| f.entry.path.as_str()).collect()
    }

    #[test]
    fn partition_groups_similar_fingerprints_and_keeps_unknowns_together() {
        let [x, y, z] = [
            0x0f0f_0f0f_0f0f_0f0fu64,
            0xf0f0_f0f0_f0f0_f0f0,
            0x3333_cccc_3333_cccc,
        ];
        let video = |v: u64| Some(Fingerprint::Video(vec![v; 3]));
        let files = ["a", "b", "c", "d", "e", "f"].map(file).to_vec();
        let control = ScanControl::default();
        let groups = partition(
            files,
            vec![video(x), None, video(y), video(x ^ 1), None, video(z)],
            &control,
        )
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(paths(&groups[0]), ["a", "d"]);
        assert!(matches!(groups[0].evidence, MatchEvidence::VideoFrames));
        assert_eq!(paths(&groups[1]), ["b", "e"]);
        assert!(matches!(groups[1].evidence, MatchEvidence::Duration));
        assert!(control.warnings().is_empty());

        let words = noise(20);
        let audio = || Some(Fingerprint::Audio(vec![words.clone(); 3]));
        let groups = partition(
            vec![file("a"), file("b"), file("c")],
            vec![audio(), None, audio()],
            &control,
        )
        .unwrap();
        assert_eq!(groups.len(), 1, "a lone unknown is not a group");
        assert_eq!(paths(&groups[0]), ["a", "c"]);
        assert!(matches!(
            groups[0].evidence,
            MatchEvidence::AudioFingerprint
        ));
    }

    #[test]
    fn partition_stops_comparing_after_256_distinct_fingerprints() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut distinct = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Some(Fingerprint::Video(vec![x; 3]))
        };
        let mut fingerprints: Vec<_> = (0..256).map(|_| distinct()).collect();
        let late = distinct();
        fingerprints.extend([late.clone(), late]);
        let files = (0..258).map(|i| file(&i.to_string())).collect();
        let control = ScanControl::default();
        let groups = partition(files, fingerprints, &control).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(paths(&groups[0]), ["256", "257"]);
        assert!(matches!(groups[0].evidence, MatchEvidence::Duration));
        assert_eq!(control.warnings().len(), 2);
    }

    #[test]
    fn partition_and_refinement_honour_cancellation_and_empty_input() {
        let control = ScanControl::default();
        let groups = refine_groups(vec![], &control, |_, _| panic!("no work, so no progress"));
        assert!(groups.unwrap().is_empty());
        assert!(control.warnings().is_empty(), "empty input needs no FFmpeg");
        control.cancelled.store(true, Ordering::SeqCst);
        let error = partition(vec![file("a")], vec![None], &control)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    #[ignore = "requires ffmpeg"]
    fn real_unreadable_media_stays_a_duration_only_group() {
        let dir = TestDir::new();
        let files: Vec<_> = ["a.mp4", "b.mp4"]
            .into_iter()
            .map(|name| DuplicateFile {
                entry: FileEntry {
                    path: dir.file(name, b"not a video"),
                    size: 11,
                    modified: None,
                },
                media: Some(crate::model::MediaInfo {
                    kind: MediaKind::Video,
                    duration_secs: 10.0,
                    width: None,
                    height: None,
                    codec: None,
                }),
            })
            .collect();
        let control = ScanControl::default();
        let progress = std::sync::Mutex::new(Vec::new());
        let groups = refine_groups(
            vec![DuplicateGroup {
                files,
                evidence: MatchEvidence::Duration,
                reclaimable_bytes: 0,
            }],
            &control,
            |done, total| progress.lock().unwrap().push((done, total)),
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        assert!(matches!(groups[0].evidence, MatchEvidence::Duration));
        let warnings = control.warnings();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .all(|w| w.contains("Content comparison unavailable"))
        );
        let mut progress = progress.into_inner().unwrap();
        progress.sort();
        assert_eq!(progress, [(0, 2), (1, 2), (2, 2)]);
    }

    proptest::proptest! {
        #[test]
        fn chromaprint_parsing_never_panics_on_arbitrary_output(raw in proptest::collection::vec(proptest::num::u8::ANY, 0..200)) {
            if let Some(words) = chromaprint_hashes(&raw) {
                proptest::prop_assert_eq!(words.len() * 4, raw.len());
            }
        }

        #[test]
        fn frame_hashing_never_panics_on_arbitrary_output(pixels in proptest::collection::vec(proptest::num::u8::ANY, 0..100)) {
            if let Some(hash) = frame_hash(&pixels) {
                proptest::prop_assert!((8..=56).contains(&hash.count_ones()));
            }
        }
    }
}
