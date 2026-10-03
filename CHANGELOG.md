# Changelog

## 0.6.0 - 2026-10-03

- Make full-content verification before trash/permanent removal optional and off by default; retain metadata and retained-copy checks.

- Add optional sampled video-frame and Chromaprint audio comparisons with explicit evidence labels and conservative fallbacks.
- Page large result groups and issue lists; throttle scan progress and bound media process concurrency.
- Recheck both selected and retained files at the removal boundary; cache retained-file verification within a batch.
- Add 100,000-file scale validation, real re-encoding fixtures, native trash/restore tests, and a Linux desktop startup smoke check.
- Configure Linux, Windows, and macOS CI and record local results and remaining validation in `docs/validation.md`.

- Deduplicate canonical paths across overlapping scan roots.
- Recheck metadata before removal, optionally verify full content, and require an unchanged retained group member.
- Restrict permanent-delete fallback to failed trash operations, with fresh verification.
- Prevent overlapping scans/removals and lock frontend actions during deletion.
- Bound duration groups by their full spread; label them as comparisons and exclude them from reclaimable space.
- Add scan cancellation, bounded ffprobe execution, and visible per-file scan and removal errors.
- Add Rust safety regressions and browser tests for cancellation, errors, and deletion flows.

## 0.5.0 - 2026-07-21

### Added

- Borderless window with a custom titlebar (minimize, maximize/restore, close, drag-to-move, double-click-to-maximize).

## 0.4.0 - 2026-07-21

### Added

- Play button on video/audio duplicate rows, opening the file with the system's default player.

## 0.3.0 - 2026-07-21

### Added

- Permanent-delete fallback for files whose location doesn't support a trash/recycle bin (network shares, NAS mounts), gated behind an explicit confirmation.
- `LICENSE` (MIT) and a CI workflow running `cargo test` and `cargo build` on push/PR.

### Changed

- `trash_files` now reports per-file failures instead of aborting the whole batch on the first untrashable path.
- Trash/permanent-delete confirmations use the native dialog plugin instead of the browser's `confirm()`.
- Moved result formatting (sizes, durations, dates, group headers) and post-trash group recomputation from the frontend into the Rust backend; the webview now just renders precomputed fields.

### Tests

- Added scanner coverage for minimum file size and hidden file/folder filtering.
- Added duration-clustering coverage for tolerance boundaries, lone files, mixed audio/video, and excluded (already-exact-duplicate) paths.
- Added coverage for the new formatting helpers and post-removal group recomputation.

## 0.2.0 - 2026-07-21

### Added

- Drag-and-drop folder selection.
- Double-click file opening.
- Native file context menu with Open, Show in folder, and Select/Unselect actions.

### Changed

- Replaced compile-time FFmpeg linking with optional runtime `ffprobe` detection for Windows compatibility.
- Simplified the desktop crate layout and duplicate-group response data.
- Reduced exact duplicate I/O with sampled prefiltering before full verification.
- Removed avoidable media/path cloning and made selection totals linear-time.
- Updated platform setup instructions.

### Security

- Restricted file opening and trash operations to files returned by the latest scan.
