# Duplicate Finder

![Version](https://img.shields.io/badge/version-0.1.0-blue)
[![Rust 2024](https://img.shields.io/badge/Rust-2024-orange?logo=rust)](https://www.rust-lang.org/)
[![Tauri 2](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![BLAKE3](https://img.shields.io/badge/hashing-BLAKE3-5E4AE3)](https://github.com/BLAKE3-team/BLAKE3)
[![FFmpeg](https://img.shields.io/badge/media-FFmpeg-007808?logo=ffmpeg&logoColor=white)](https://ffmpeg.org/)

Duplicate Finder is a desktop app for finding exact and likely duplicate files. It combines byte-for-byte content hashing with media-duration matching, making it useful for spotting copies of videos or audio files that have been re-encoded at a different resolution, bitrate, or file size.

Built with Rust, Tauri 2, and a lightweight HTML/CSS/JavaScript interface.

## Features

- Scan one or more folders recursively.
- Find exact duplicates using full-file BLAKE3 hashes.
- Avoid unnecessary work by hashing only files that share a file size.
- Find likely duplicate audio and video files by duration.
- Configure media-duration tolerance from 0 to 5 seconds.
- Exclude small files and hidden files or folders.
- View live scanning, probing, and hashing progress.
- Review file sizes, dates, codecs, resolutions, and durations.
- Select unwanted copies and move them to the operating system's trash or recycle bin.

Duration matches are suggestions, not proof that two files contain the same content. Review likely matches before moving anything to the trash.

## Requirements

- [Rust](https://www.rust-lang.org/tools/install)
- Tauri 2 system dependencies for your operating system
- FFmpeg development libraries
- The Tauri CLI

Install the Tauri CLI with:

```sh
cargo install tauri-cli --version "^2"
```

### Debian or Ubuntu

Install the Tauri and FFmpeg development dependencies with:

```sh
sudo apt update
sudo apt install build-essential curl file wget libxdo-dev \
  libayatana-appindicator3-dev \
  libavcodec-dev libavdevice-dev libavfilter-dev libavformat-dev \
  libavutil-dev libssl-dev libswresample-dev libswscale-dev \
  libwebkit2gtk-4.1-dev librsvg2-dev
```

Package names differ between Linux distributions.

### macOS

Install Apple's command-line developer tools:

```sh
xcode-select --install
```

Then install FFmpeg and `pkg-config` with [Homebrew](https://brew.sh/):

```sh
brew install ffmpeg pkg-config
```

Full Xcode is not required for desktop-only development, but it is required if you intend to target iOS.

### Windows

1. Install [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) and select the **Desktop development with C++** workload.
2. Install the [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/). It is already present on current Windows 10 and Windows 11 installations.
3. Install Rust with the MSVC toolchain, then confirm it with:

   ```powershell
   rustup default stable-msvc
   ```

4. Install FFmpeg development libraries using [vcpkg](https://github.com/microsoft/vcpkg):

   ```powershell
   git clone https://github.com/microsoft/vcpkg.git C:\vcpkg
   C:\vcpkg\bootstrap-vcpkg.bat
   C:\vcpkg\vcpkg.exe install ffmpeg:x64-windows
   [Environment]::SetEnvironmentVariable("VCPKG_ROOT", "C:\vcpkg", "User")
   ```

Restart the terminal after setting `VCPKG_ROOT`. If an MSI build fails while running `light.exe`, enable the **VBSCRIPT** Windows optional feature; Tauri needs it when the bundle target is `msi` or `all`.

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
4. Review exact duplicates and likely media matches separately.
5. Select unwanted copies and choose **Move to trash**.

Files are sent to the system trash rather than permanently deleted, but it is still worth checking paths and likely-duration matches carefully.

## How matching works

Exact matches are grouped by file size and then hashed in parallel with BLAKE3. Files with the same size and hash are byte-for-byte identical.

For supported media extensions, FFmpeg reads duration and available codec or resolution metadata. Audio and video files are grouped separately when adjacent durations fall within the selected tolerance. Files already reported as exact duplicates are excluded from likely-match groups.

If FFmpeg cannot initialize, exact duplicate scanning remains available and duration matching is skipped.

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

No license has been specified yet.
