# Duplicate Finder

[![Latest tag](https://img.shields.io/github/v/tag/gregorycarnegie/duplicate_finder?label=version)](https://github.com/gregorycarnegie/duplicate_finder/tags)
[![CI](https://github.com/gregorycarnegie/duplicate_finder/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/gregorycarnegie/duplicate_finder/actions/workflows/ci.yml)
[![Rust 2024](https://img.shields.io/badge/Rust-2024-orange?logo=rust)](https://www.rust-lang.org/)
[![Tauri 2](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![BLAKE3](https://img.shields.io/badge/hashing-BLAKE3-5E4AE3)](https://github.com/BLAKE3-team/BLAKE3)
[![FFmpeg](https://img.shields.io/badge/media-FFmpeg-007808?logo=ffmpeg&logoColor=white)](https://ffmpeg.org/)

Duplicate Finder is a desktop app for finding exact duplicate files and media files worth comparing. It combines full-content BLAKE3 hashing with media-duration matching and optional sampled video-frame or audio-fingerprint comparisons to help find re-encoded copies.

Built with Rust, Tauri 2, and a lightweight HTML/CSS/JavaScript interface.

## Features

- Scan one or more folders recursively.
- Add scan folders with the picker or drag and drop.
- Find exact duplicates using full-file BLAKE3 hashes.
- Prefilter exact-duplicate candidates by size and sampled content before full hashing.
- Find audio and video files with similar durations for manual comparison.
- Optionally compare sampled video frames and audio fingerprints across re-encodings.
- Configure media-duration tolerance from 0 to 5 seconds.
- Exclude small files and dot-prefixed files or folders.
- View live scanning, probing, hashing, media comparison, and verification progress; cancel a scan.
- Review scan issues for unreadable files, failed media probes, and files that changed.
- Scan overlapping folders without counting the same canonical file path twice.
- Review file sizes, dates, codecs, resolutions, and durations.
- Browse large result sets in pages, with selections preserved between pages.
- Double-click a file path to open it in its default application.
- Select unwanted copies and move them to the operating system's trash or recycle bin.
- Remove files with quick metadata checks by default; optionally recheck full file contents before removal.

Media matches are review suggestions. Duration and sampled content cannot prove that entire recordings are equivalent; video frame comparisons do not compare soundtracks. Review matches before moving anything to the trash.

## Requirements

- Current stable [Rust](https://www.rust-lang.org/tools/install); the crate uses edition 2024.
- Tauri 2 system dependencies for your operating system
- The Tauri CLI

FFmpeg is optional. Exact duplicate scanning works without it. Put `ffprobe` on `PATH` for duration and codec metadata; put `ffmpeg` on `PATH` for sampled content comparisons. Audio comparisons also require an FFmpeg build with Chromaprint support.

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

Clone the repository first:

```sh
git clone https://github.com/gregorycarnegie/duplicate_finder.git
cd duplicate_finder
```

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
2. Choose a duration tolerance, minimum file size, and whether dot-prefixed files should be included. Enable **Compare media content (slower)** if you want sampled comparisons; it is off by default.
3. Start the scan.
4. Review exact duplicates and media comparison groups separately; each media group states its matching evidence.
5. Select unwanted copies, leaving at least one file in each group. Leave **Verify file contents before removal** off for faster removal, or enable it for a full content recheck.
6. Choose **Move to trash** and confirm.

Files are sent to the system trash rather than permanently deleted, but it is still worth checking paths and likely-duration matches carefully. Paths that don't support a trash/recycle bin (network shares, NAS mounts, some removable drives) will prompt for a permanent delete instead — that action cannot be undone.

Before removal, the app rechecks file metadata for the selected file and a retained member of its group. **Verify file contents before removal** is optional and off by default: enable it to also reread and compare full-content hashes, which can be slow for large files or network drives. With it off, content changes that leave the recorded metadata unchanged are not detected. Detected changes block removal and require a new scan. At least one unchanged file must remain in each group. Safety-check failures never offer permanent deletion; only failed trash operations can do so, with a separate confirmation and fresh checks using the same verification setting. Scans and removal operations cannot run concurrently.

File checks and the final operating-system removal are separate operations, so concurrent changes by another application cannot be ruled out completely. Avoid editing or moving files while removing duplicates.

## How matching works

Exact-duplicate candidates are grouped by file size. For files larger than 128 KiB, the first and last 64 KiB are hashed as a prefilter; candidates whose samples match then receive a full BLAKE3 hash. Candidates of 128 KiB or smaller are hashed in full immediately. Exact results require matching sizes and full-content hashes.

For supported media extensions, `ffprobe` reads duration and available codec or resolution metadata. Audio and video files are grouped separately, with the entire duration spread of each group bounded by the selected tolerance. Duration-only groups are comparison suggestions and are excluded from the reclaimable-space total. Files already reported as exact duplicates are excluded from media comparison groups.

Enable **Compare media content (slower)** to refine duration groups using sampled video frame hashes or Chromaprint audio fingerprints. This needs `ffmpeg` on `PATH`; audio also needs a build with [Chromaprint support](https://ffmpeg.org/ffmpeg-formats.html#chromaprint). Missing support, low-detail samples, and failed decoding appear as scan issues. These comparisons remain suggestions and never contribute to reclaimable space. See the [validation record](docs/validation.md) for thresholds, tested fixtures, and limitations.

Probing is limited to four workers and content extraction to two, and progress updates are throttled to avoid flooding the desktop UI.

Before displaying results, the scan records metadata and a full-content hash for each result file, reusing hashes already computed for exact matching. This can add full-file reads for media comparison results. The removal verification checkbox controls only the later removal step.

If `ffprobe` cannot initialize within five seconds, exact duplicate scanning remains available and duration matching is skipped. Individual probes time out after 30 seconds and appear in the scan issues. Cancellation stops active probes and is checked between filesystem operations and hash chunks; a blocked filesystem call must return before cancellation can finish.

## Tests

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
python3 tests/run_frontend.py
```

CI is configured to build and run unit/native-trash tests on Linux, Windows, and macOS; Linux also runs browser, real-media, and desktop-startup checks. The badge above links to the current CI status. See the [validation record](docs/validation.md) for which checks have actually run, scale measurements, and opt-in test commands.

Frontend tests require Chrome/Chromium (or `CHROME_BIN` pointing to it) and Python 3. They run the actual HTML and JavaScript in a headless browser with mocked Tauri commands, without touching user files. Rust regression tests cover overlapping roots, changed-file protection, retained-copy checks, partial failures, cancellation, and probe timeouts.

## Project structure

```text
.
├── docs/                  # Validation results and known limitations
├── tests/                 # Browser regressions and desktop startup check
├── ui/                    # HTML, CSS, and JavaScript interface
└── src-tauri/
    ├── capabilities/      # Tauri permissions
    ├── icons/             # Application icons
    └── src/               # Rust scanning and desktop application code
```

## License

[MIT](LICENSE)
