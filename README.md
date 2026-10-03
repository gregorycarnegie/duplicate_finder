# Duplicate Finder

![Version](https://img.shields.io/badge/version-0.6.0-blue)
[![Rust 2021](https://img.shields.io/badge/Rust-2021-orange?logo=rust)](https://www.rust-lang.org/)
[![Tauri 2](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![BLAKE3](https://img.shields.io/badge/hashing-BLAKE3-5E4AE3)](https://github.com/BLAKE3-team/BLAKE3)
[![FFmpeg](https://img.shields.io/badge/media-FFmpeg-007808?logo=ffmpeg&logoColor=white)](https://ffmpeg.org/)

Duplicate Finder is a desktop app for finding exact duplicate files and media files worth comparing. It combines byte-for-byte content hashing with media-duration matching, making it useful for spotting copies of videos or audio files that have been re-encoded at a different resolution, bitrate, or file size.

Built with Rust, Tauri 2, and a lightweight HTML/CSS/JavaScript interface.

## Features

- Scan one or more folders recursively.
- Add scan folders with the picker or drag and drop.
- Find exact duplicates using full-file BLAKE3 hashes.
- Avoid unnecessary work by hashing only files that share a file size.
- Find audio and video files with similar durations for manual comparison.
- Optionally compare sampled video frames and audio fingerprints across re-encodings.
- Configure media-duration tolerance from 0 to 5 seconds.
- Exclude small files and hidden files or folders.
- View live scanning, probing, hashing, and verification progress; cancel a scan.
- Review scan issues for unreadable files, failed media probes, and files that changed.
- Scan overlapping folders without counting the same canonical file path twice.
- Review file sizes, dates, codecs, resolutions, and durations.
- Browse large result sets in pages, with selections preserved between pages.
- Double-click a file path to open it in its default application.
- Select unwanted copies and move them to the operating system's trash or recycle bin.

Media matches are review suggestions. Duration and sampled content cannot prove that entire recordings are equivalent; video frame comparisons do not compare soundtracks. Review matches before moving anything to the trash.

## Requirements

- [Rust](https://www.rust-lang.org/tools/install)
- Tauri 2 system dependencies for your operating system
- FFmpeg (`ffprobe` must be on `PATH` for duration-based matching)
- The Tauri CLI

Install the Tauri CLI with:

```sh
cargo install tauri-cli --version "^2"
```

### Debian or Ubuntu

Install the Tauri dependencies and FFmpeg with:

```sh
sudo apt update
sudo apt install build-essential curl file wget libxdo-dev \
  libayatana-appindicator3-dev libssl-dev \
  libwebkit2gtk-4.1-dev librsvg2-dev ffmpeg
```

Package names differ between Linux distributions.

### macOS

Install Apple's command-line developer tools:

```sh
xcode-select --install
```

Then install FFmpeg with [Homebrew](https://brew.sh/):

```sh
brew install ffmpeg
```

Full Xcode is not required for desktop-only development, but it is required if you intend to target iOS.

### Windows

1. Install [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) and select the **Desktop development with C++** workload.
2. Install the [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/). It is already present on current Windows 10 and Windows 11 installations.
3. Install Rust with the MSVC toolchain, then confirm it with:

   ```powershell
   rustup default stable-msvc
   ```

4. Install an [FFmpeg Windows build](https://ffmpeg.org/download.html#build-windows) and add its `bin` directory to `PATH`.

   Verify the installation in a new terminal with `ffprobe -version`. FFmpeg is optional: without it, exact duplicate scanning still works and duration-based matching is skipped. If an MSI build fails while running `light.exe`, enable the **VBSCRIPT** Windows optional feature; Tauri needs it when the bundle target is `msi` or `all`.

See the [official Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for additional platforms and troubleshooting.

## Run in development

From the project root:

```sh
cd src-tauri
cargo tauri dev
```

The frontend is served directly from `ui/`, so there is no Node.js install or frontend build step.

## Build

To create a release build and platform installer:

```sh
cd src-tauri
cargo tauri build
```

Generated bundles are written below `src-tauri/target/release/bundle/`.

## Usage

1. Add one or more folders to scan.
2. Choose a duration tolerance, minimum file size, and whether hidden files should be included.
3. Start the scan.
4. Review exact duplicates and media comparison groups separately; each media group states its matching evidence.
5. Select unwanted copies and choose **Move to trash**.

Files are sent to the system trash rather than permanently deleted, but it is still worth checking paths and likely-duration matches carefully. Paths that don't support a trash/recycle bin (network shares, NAS mounts, some removable drives) will prompt for a permanent delete instead — that action cannot be undone.

Before removal, the app rechecks file metadata for the selected file and a retained member of its group. **Verify file contents before removal** is optional and off by default: enable it to also reread and compare full-content hashes, which can be slow for large files or network drives. With it off, content changes that leave the recorded metadata unchanged are not detected. Detected changes block removal and require a new scan. At least one unchanged file must remain in each group. Safety-check failures never offer permanent deletion; only failed trash operations can do so, with a separate confirmation and fresh checks using the same verification setting. Scans and removal operations cannot run concurrently.

File checks and the final operating-system removal are separate operations, so concurrent changes by another application cannot be ruled out completely. Avoid editing or moving files while removing duplicates.

## How matching works

Exact matches are grouped by file size and then hashed in parallel with BLAKE3. Files with the same size and hash are byte-for-byte identical.

For supported media extensions, FFmpeg reads duration and available codec or resolution metadata. Audio and video files are grouped separately, with the entire duration spread of each group bounded by the selected tolerance. Duration-only groups are comparison suggestions and are excluded from the reclaimable-space total. Files already reported as exact duplicates are excluded from likely-match groups.

Enable **Compare media content (slower)** to refine duration groups using sampled video frame hashes or Chromaprint audio fingerprints. This needs `ffmpeg` on `PATH`; audio also needs a build with [Chromaprint support](https://ffmpeg.org/ffmpeg-formats.html#chromaprint). Missing support, low-detail samples, and failed decoding appear as scan issues. These comparisons remain suggestions and never contribute to reclaimable space. See the [validation record](docs/validation.md) for thresholds, tested fixtures, and limitations.

Probing is limited to four workers and content extraction to two, and progress updates are throttled to avoid flooding the desktop UI.

If FFmpeg cannot initialize within five seconds, exact duplicate scanning remains available and duration matching is skipped. Individual probes time out after 30 seconds and appear in the scan issues. Cancellation stops active probes and is checked between filesystem operations and hash chunks; a blocked filesystem call must return before cancellation can finish.

## Tests

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
python3 tests/run_frontend.py
```

CI is configured to build and run unit/native-trash tests on Linux, Windows, and macOS. See the [validation record](docs/validation.md) for which checks have actually run, scale measurements, and opt-in test commands.

Frontend tests require Chrome/Chromium (or `CHROME_BIN` pointing to it) and Python 3. They run the actual HTML and JavaScript in a headless browser with mocked Tauri commands, without touching user files. Rust regression tests cover overlapping roots, changed-file protection, retained-copy checks, partial failures, cancellation, and probe timeouts.

## Project structure

```text
.
├── ui/                    # HTML, CSS, and JavaScript interface
└── src-tauri/
    ├── capabilities/      # Tauri permissions
    ├── icons/             # Application icons
    └── src/               # Rust scanning and desktop application code
```

## License

[MIT](LICENSE)
