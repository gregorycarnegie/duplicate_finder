# Changelog

## Unreleased

### Added

- Drag-and-drop folder selection.
- Double-click file opening.
- Native file context menu with Open, Show in folder, and Select/Unselect actions.

### Changed

- Replaced compile-time FFmpeg linking with optional runtime `ffprobe` detection for Windows compatibility.
- Simplified the desktop crate layout and duplicate-group response data.
- Updated platform setup instructions.

### Security

- Restricted file opening and trash operations to files returned by the latest scan.
