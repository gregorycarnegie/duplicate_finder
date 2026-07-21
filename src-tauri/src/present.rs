//! Turns the raw scan model into display-ready strings, so the frontend
//! only has to drop text into the DOM (formatting logic used to be
//! duplicated in ui/app.js).
use crate::model::{DuplicateFile, DuplicateGroup, MediaInfo, ScanSummary};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn format_bytes(n: u64) -> String {
    if n == 0 {
        return "0 B".to_string();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if unit == 0 || value >= 10.0 { 0 } else { 1 };
    format!("{value:.decimals$} {}", UNITS[unit])
}

pub fn format_duration(secs: f64) -> String {
    let total_ms = (secs * 1000.0).round() as i64;
    let ms = total_ms % 1000;
    let total_sec = total_ms / 1000;
    let s = total_sec % 60;
    let total_min = total_sec / 60;
    let m = total_min % 60;
    let h = total_min / 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

pub fn format_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Days-since-epoch -> (year, month, day), via Howard Hinnant's
/// civil_from_days algorithm. No date/time crate needed for this.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn format_date(unix_secs: Option<i64>) -> String {
    let Some(secs) = unix_secs else {
        return String::new();
    };
    let (year, month, day) = civil_from_days(secs.div_euclid(86400));
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (now_year, _, _) = civil_from_days(now_secs.div_euclid(86400));

    let month_name = MONTHS[(month - 1) as usize];
    if year == now_year {
        format!("{month_name} {day}")
    } else {
        format!("{month_name} {day}, {year}")
    }
}

fn media_badge(media: &MediaInfo) -> String {
    let dims = match (media.width, media.height) {
        (Some(w), Some(h)) => format!("{w}\u{d7}{h} "),
        _ => String::new(),
    };
    let codec = media
        .codec
        .as_ref()
        .map(|c| format!("{} ", c.to_uppercase()))
        .unwrap_or_default();
    format!("{dims}{codec}{}", format_duration(media.duration_secs))
}

#[derive(Clone, Copy)]
enum GroupKind {
    Exact,
    Media,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub path: String,
    pub size: u64,
    pub size_text: String,
    pub detail_text: String,
    pub playable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupView {
    pub files: Vec<FileView>,
    pub header_left: String,
    pub header_right: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummaryView {
    pub exact_groups: Vec<GroupView>,
    pub media_groups: Vec<GroupView>,
    pub files_scanned_text: String,
    pub reclaimable_text: String,
    pub elapsed_text: String,
    pub ffmpeg_available: bool,
}

fn present_file(file: &DuplicateFile) -> FileView {
    FileView {
        path: file.entry.path.clone(),
        size: file.entry.size,
        size_text: format_bytes(file.entry.size),
        detail_text: file
            .media
            .as_ref()
            .map(media_badge)
            .unwrap_or_else(|| format_date(file.entry.modified)),
        playable: file.media.is_some(),
    }
}

fn group_header_left(group: &DuplicateGroup, kind: GroupKind) -> String {
    let n = group.files.len();
    let noun = if n == 1 { "file" } else { "files" };
    match kind {
        GroupKind::Exact => {
            let size = group.files.first().map(|f| f.entry.size).unwrap_or(0);
            format!("{n} {noun} \u{b7} {} each", format_bytes(size))
        }
        GroupKind::Media => {
            let durations: Vec<f64> = group
                .files
                .iter()
                .filter_map(|f| f.media.as_ref().map(|m| m.duration_secs))
                .collect();
            let spread = durations.iter().cloned().fold(f64::MIN, f64::max)
                - durations.iter().cloned().fold(f64::MAX, f64::min);
            format!("{n} {noun} \u{b7} duration spread {spread:.2}s")
        }
    }
}

fn present_group(group: &DuplicateGroup, kind: GroupKind) -> GroupView {
    GroupView {
        header_left: group_header_left(group, kind),
        header_right: format!("{} reclaimable", format_bytes(group.reclaimable_bytes)),
        files: group.files.iter().map(present_file).collect(),
    }
}

pub fn present_summary(summary: &ScanSummary) -> ScanSummaryView {
    ScanSummaryView {
        exact_groups: summary
            .exact_groups
            .iter()
            .map(|g| present_group(g, GroupKind::Exact))
            .collect(),
        media_groups: summary
            .media_groups
            .iter()
            .map(|g| present_group(g, GroupKind::Media))
            .collect(),
        files_scanned_text: format!("{} files scanned", format_count(summary.files_scanned)),
        reclaimable_text: format!("{} reclaimable", format_bytes(summary.reclaimable_bytes)),
        elapsed_text: format!("finished in {}", format_ms(summary.elapsed_ms)),
        ffmpeg_available: summary.ffmpeg_available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_scale_to_the_right_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10 MB");
    }

    #[test]
    fn durations_include_hours_only_when_needed() {
        assert_eq!(format_duration(65.25), "1:05.250");
        assert_eq!(format_duration(3665.0), "1:01:05.000");
    }

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(format_count(7), "7");
        assert_eq!(format_count(1234), "1,234");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    #[test]
    fn millis_switch_to_seconds_past_one_thousand() {
        assert_eq!(format_ms(250), "250ms");
        assert_eq!(format_ms(1500), "1.5s");
    }
}
