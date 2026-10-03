# Validation record

This records local and CI checks and their limits. It is not a claim that the app is production-ready on every platform.

## Test-suite and mutation-testing pass — 2026-10-03

Environment: Windows 11, Rust 1.99.0, FFmpeg 8.1.2 (with Chromaprint), Chrome, debug/test builds.

| Check | Result | Scope |
| --- | --- | --- |
| Rust default suite | 95 passed, 7 opt-in tests skipped | Adds IPC tests on Tauri's mock runtime (`src-tauri/src/ipc_tests.rs`): every UI command invoked with the JSON `ui/app.js` sends, checked against the fields the UI reads |
| Real media tests (`tests::real_`) | 4 passed | Adds a probe test that reads the requested stream from a file holding both video and audio |
| Browser suite | 19 passed | Now loads Tauri's own JS API and dialog plugin at the `Cargo.lock` versions and uses Tauri's official `mockIPC`/`mockWindows`, instead of a hand-written `window.__TAURI__` stub; adds progress events, drag-drop, context menu, play/open, titlebar and folder-list tests |
| Mutation testing (cargo-mutants 27.1.0) | 381 caught, 4 missed, 0 timeouts (99%) | Up from 82.4% before this pass; the 4 survivors are equivalent mutants, with reasons recorded in `.cargo/mutants.toml` |
| Clippy, rustfmt and application build | Passed | All targets, warnings denied |

Bugs found and fixed:

- Scan progress sent `files_found` while the UI reads `filesFound`, so the scanning screen's "files found" counter threw on every walking event and never updated (present since the initial commit). Found by the IPC progress test.
- The progress throttle could drop the final walking count when the walk ended within 100 ms of the previous event, leaving the counter at the last multiple of 100. Found by a surviving mutant.

Test builds now stub the OS trash and the shell's open/reveal, so neither tests nor mutants can fill the real trash or pop shell dialogs. `native_trash_roundtrip` still covers the real trash call. Fingerprint partitioning, Chromaprint parsing and sample timing moved into pure functions so they can be tested without FFmpeg; property tests check that the ffprobe, frame and Chromaprint parsers never panic on arbitrary output. The ffprobe parser is also pinned to output captured from a real ffprobe 8.1.2.

## v0.6.0 release checks — 2026-10-03

Local validation before tagging v0.6.0 passed: 54 Rust tests (5 opt-in tests skipped), 12 browser tests, Clippy with warnings denied, and the application build. The new regressions cover optional content verification for both trash and permanent deletion, including retained metadata checks.

[Release CI run for commit 2de00a1](https://github.com/gregorycarnegie/duplicate_finder/actions/runs/37105897528):

| Platform | Result |
| --- | --- |
| Linux | Passed unit tests, native trash/restore, real-media comparisons, browser tests, build, and desktop startup |
| macOS | Passed unit tests, native trash/restore, and build |
| Windows | Unit tests passed; native trash/restore failed because its generated fixture was not found in the recycle-bin listing; the subsequent build step was skipped |

The Windows native-trash failure was a test bug, not a trash bug. The fixture was recycled every time, but `trash` lists Windows items by Explorer display name, which drops the extension when Explorer hides known file types (the hosted runner's default), so the test's exact-path lookup missed it. Confirmed by toggling `HideFileExt` on a runner. `restore_all` has the same flaw and restores the item without its extension, so the fixture now has no extension and is matched by its unique folder. The app only calls `trash::delete`, which is unaffected. These are results for the release commit; see [current CI runs](https://github.com/gregorycarnegie/duplicate_finder/actions/workflows/ci.yml) for later commits.

## Earlier local checks — 2026-09-19

Environment: Linux, local ZFS storage, Rust 1.97.1, FFmpeg 6.1.1, debug/test builds.

| Check | Result | Scope |
| --- | --- | --- |
| Rust default suite | 52 passed, 5 opt-in tests skipped | Includes changed files, retained-copy races, partial failures, permissions, cancellation, timeouts, and progress throttling |
| Browser suite | 11 passed | Real HTML/JS in Chrome, mocked Tauri IPC |
| Real video comparison | Passed | Generated 12-second video matched its resized/lossy re-encoding; mirrored video did not match |
| Real audio comparison | Passed | Generated 24-second changing-note audio matched its MP3 encoding; a different note sequence did not match |
| Native Linux trash/restore | Passed | Production removal validation moved one generated fixture to real trash, restored it, and kept the other copy intact |
| Native Linux startup | Passed | Desktop process stayed alive for 5 seconds under Xvfb; does not exercise native dialogs |
| Clippy and application build | Passed | All targets checked with warnings denied; application built locally |

The five opt-in Rust tests were also run separately: two real-media tests, native trash/restore, the 100,000-file scale test, and the existing hashing benchmark.

## Scale measurements

The synthetic scale test creates 100,000 eight-byte files across 100 folders, forming exactly 1,000 groups of 100 equal files. It scans both the parent and one overlapping child root and asserts that files and groups are counted once.

Last measured run:

- Fixture creation: 6.507 seconds.
- Walking/metadata collection: 1.416 seconds.
- Exact duplicate hashing/grouping: 1.145 seconds.
- Peak resident memory of the test executable: 126,516 KiB (about 124 MiB).
- Total test runtime including setup, assertions, and cleanup: 12.77 seconds.

Memory was measured by running the already-built test executable directly under `/usr/bin/time -v`; compiler memory is excluded. These are warm-cache, small-file measurements. They exclude media decoding, Tauri IPC, result snapshot verification, and frontend serialization. They do not predict cold disk, HDD, NAS, or large-video throughput.

The existing 8 MiB/file benchmark also passed. For its warm-cache mostly-distinct fixture, average full hashing took 39.22 ms versus 10.78 ms with sampling; for its all-identical fixture, full hashing took 27.20 ms versus 27.76 ms with sampling. This demonstrates both the benefit and the overhead of sampling on these particular fixtures, not general disk throughput.

The browser suite supplies 100,000 result files and asserts that only the first 50 rows of a group are initially rendered, that subsequent pages work, and that selections survive paging. A separate test covers 1,000 groups and 1,000 issues. Timing from Chrome's virtual test clock is deliberately not reported as a performance measurement.

## Reproduce

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path src-tauri/Cargo.toml --locked
python3 tests/run_frontend.py
cargo test --manifest-path src-tauri/Cargo.toml --locked large_library_scan -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --locked benchmark_exact_duplicate_scan -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --locked tests::real_ -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --locked native_trash_roundtrip -- --ignored --nocapture
python3 tests/smoke_desktop.py
cargo mutants -j4    # from the repo root; settings in .cargo/mutants.toml
```

The browser suite finds Chrome or Chromium on `PATH`, or takes `CHROME_BIN`. A mutation run takes about 15 minutes on a 32-thread machine and needs FFmpeg with Chromaprint, because it includes the real-media tests.

`DUPLICATE_FINDER_STRESS_FILES` can override the scale fixture count (at least 100, divisible by 100). Native trash tests touch only their uniquely named generated fixtures. The native startup script requires Linux, Xvfb, and `dbus-run-session`. Real audio tests require FFmpeg with Chromaprint and MP3 encoding support.

## Media comparison design and limits

“Compare media content” is optional because it requires additional decoding. Video comparisons use 64-bit gradient hashes from frames at 20%, 50%, and 80% of the duration. Low-detail frames are rejected. Audio comparisons use three segments of up to 15 seconds with [FFmpeg's Chromaprint muxer](https://ffmpeg.org/ffmpeg-formats.html#chromaprint); short or repetitive fingerprints are rejected. The raw representation follows [FFmpeg's muxer implementation](https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/chromaprint.c).

Every sample must meet its comparison threshold against a group's reference fingerprint. Video allows at most 8 differing bits per 64-bit frame hash. Audio allows at most 10% differing bits, at least 80% overlap, and a small alignment offset. These are application heuristics, validated on the synthetic fixtures above; they have not been calibrated against a representative media corpus.

Groups report whether their evidence is duration, sampled video frames, or sampled audio. Video samples do not compare soundtracks, and audio samples do not cover a whole long recording. Matching samples cannot prove complete equivalence. Failed extraction produces visible issues; remaining unavailable files may appear in clearly labelled duration-only groups. All media comparison groups stay outside the reclaimable-space total.

Probing uses at most four workers; content extraction uses two. Each child process has a timeout and is cancelled with the scan. A duration bucket allows at most 256 distinct reference fingerprints to bound worst-case comparison work; excess unmatched candidates produce explicit issues instead of causing unbounded pairwise comparisons.

## Remaining validation

- Where a user has turned off the Recycle Bin, Windows deletes permanently while reporting success, and the app cannot tell the difference.
- Native file pickers, confirmation dialogs, opening/revealing files, and accessibility still need hands-on checks on all supported platforms.
- No slow physical disk, remote share, disconnected NAS, or large real-world media collection was available for testing.
- Perceptual matching needs a representative labelled corpus to measure false positives and missed matches, especially crops, different edits, changed soundtracks, silence, and timing offsets.
- Metadata and content verification reduce accidental removal risk, but portable pathname-based trash/delete operations cannot make the final check and filesystem mutation atomic against arbitrary other processes.
