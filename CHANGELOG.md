# Changelog

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
