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
    for (index, row) in pixels.chunks_exact(9).enumerate() {
        for col in 0..8 {
            if row[col] > row[col + 1] {
                hash |= 1 << (index * 8 + col);
            }
        }
    }
    (8..=56).contains(&hash.count_ones()).then_some(hash)
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
        let time = match info.kind {
            MediaKind::Video => info.duration_secs * fraction,
            MediaKind::Audio => (info.duration_secs - 15.0).max(0.0) * fraction,
        };
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
                // FFmpeg's raw Chromaprint muxer writes native-endian uint32s.
                let hashes: Vec<u32> = raw
                    .chunks_exact(4)
                    .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
                    .collect();
                if raw.len() % 4 != 0
                    || hashes.len() < 16
                    || hashes.iter().collect::<HashSet<_>>().len() < 4
                {
                    return Err(io::Error::other(
                        "Audio sample is too short or repetitive for a reliable fingerprint",
                    ));
                }
                audio.push(hashes);
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
        let mut partitions: Vec<(Fingerprint, Vec<DuplicateFile>)> = Vec::new();
        let mut unavailable = Vec::new();
        for (file, fingerprint) in group.files.into_iter().zip(fingerprints) {
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
                control.warn(&file.entry.path, "Too many distinct fingerprints in this duration group; content comparison skipped");
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
}
