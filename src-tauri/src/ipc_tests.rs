//! IPC tests on Tauri's mock runtime. Each command is invoked by name with the
//! JSON body ui/app.js sends, and responses are checked against the fields the
//! UI reads, which are also the shapes tests/frontend.html mocks.
//!
//! Test builds stub the OS shell (`open_in_shell`) and see the trash as
//! unavailable (`move_to_trash`), so removal runs the same trash-failure
//! fallback the UI offers and nothing here can open a window or fill the trash.
use crate::{scan_control::Operations, test_support::TestDir};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::Ordering};
use tauri::{
    App, Listener, Manager, WebviewWindow,
    ipc::{CallbackFn, InvokeBody},
    test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets},
    webview::InvokeRequest,
};

fn ui() -> (App<MockRuntime>, WebviewWindow<MockRuntime>) {
    let app = crate::app(mock_builder())
        .build(mock_context(noop_assets()))
        .unwrap();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    (app, webview)
}

fn invoke(webview: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    let url = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.into(),
        },
    )
    .map(|body| body.deserialize().unwrap())
}

/// The `scan` arguments exactly as app.js's startScan builds them.
fn scan_args(folders: &[&str], tolerance: f64) -> Value {
    json!({ "options": {
        "folders": folders,
        "durationToleranceSecs": tolerance,
        "minFileSize": 0,
        "includeHidden": false,
        "compareMediaContent": false,
    }})
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    keys
}

/// Two 9-byte duplicates, a 9-byte different file (same size, so it is hashed
/// too) and a hidden duplicate the default options skip.
fn library() -> (TestDir, String) {
    let dir = TestDir::new();
    dir.file("a.bin", b"duplicate");
    dir.file("b.bin", b"duplicate");
    dir.file("c.bin", b"different");
    dir.file(".hidden.bin", b"duplicate");
    let folder = dir.0.to_str().unwrap().to_string();
    (dir, folder)
}

/// Scans `folder` and returns the view plus the grouped paths, sorted.
fn scan(webview: &WebviewWindow<MockRuntime>, folder: &str) -> (Value, Vec<String>) {
    let view = invoke(webview, "scan", scan_args(&[folder], 1.5)).unwrap();
    let mut paths: Vec<String> = view["exactGroups"][0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    (view, paths)
}

#[test]
fn scan_returns_the_view_the_results_screen_renders() {
    let (_dir, folder) = library();
    let (_app, webview) = ui();
    let (view, paths) = scan(&webview, &folder);
    assert_eq!(
        keys(&view),
        [
            "elapsedText",
            "exactGroups",
            "ffmpegAvailable",
            "filesScannedText",
            "mediaGroups",
            "reclaimableText",
            "warnings"
        ]
    );
    assert_eq!(view["filesScannedText"], "3 files scanned");
    assert_eq!(view["reclaimableText"], "9 B reclaimable");
    assert!(
        view["elapsedText"]
            .as_str()
            .unwrap()
            .starts_with("finished in ")
    );
    assert!(view["ffmpegAvailable"].is_boolean());
    assert_eq!(view["mediaGroups"], json!([]));
    assert_eq!(view["warnings"], json!([]));

    assert_eq!(view["exactGroups"].as_array().unwrap().len(), 1);
    let group = &view["exactGroups"][0];
    assert_eq!(keys(group), ["files", "headerLeft", "headerRight"]);
    assert_eq!(group["headerLeft"], "2 files \u{b7} 9 B each");
    assert_eq!(group["headerRight"], "9 B reclaimable");
    for (file, name) in group["files"]
        .as_array()
        .unwrap()
        .iter()
        .zip(["a.bin", "b.bin"])
    {
        assert_eq!(
            keys(file),
            ["detailText", "path", "playable", "size", "sizeText"]
        );
        assert_eq!(file["size"], 9);
        assert_eq!(file["sizeText"], "9 B");
        assert_eq!(file["playable"], false);
        assert!(!file["detailText"].as_str().unwrap().is_empty());
        assert!(
            paths.iter().any(|p| p.ends_with(name)),
            "{name} missing from {paths:?}"
        );
    }
}

#[test]
fn scan_progress_events_carry_the_fields_the_scanning_screen_reads() {
    let (_dir, folder) = library();
    let (app, webview) = ui();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    app.listen_any("scan-progress", move |event| {
        sink.lock()
            .unwrap()
            .push(serde_json::from_str::<Value>(event.payload()).unwrap());
    });
    scan(&webview, &folder);
    // Each phase's first event and its completion always pass the throttle.
    assert_eq!(
        *events.lock().unwrap(),
        [
            json!({ "phase": "walking", "folder": folder, "filesFound": 3 }),
            json!({ "phase": "hashing", "done": 3, "total": 3 }),
            json!({ "phase": "verifying", "done": 1, "total": 2 }),
            json!({ "phase": "verifying", "done": 2, "total": 2 }),
        ]
    );
}

#[test]
fn scan_rejects_invalid_options_with_the_message_the_setup_screen_shows() {
    let (dir, folder) = library();
    let (_app, webview) = ui();
    assert_eq!(
        invoke(&webview, "scan", scan_args(&[], 1.5)),
        Err(json!("Add at least one folder to scan."))
    );
    for tolerance in [-0.1, 5.1] {
        assert_eq!(
            invoke(&webview, "scan", scan_args(&[&folder], tolerance)),
            Err(json!("Duration tolerance must be between 0 and 5 seconds.")),
            "tolerance {tolerance}"
        );
    }
    let empty = dir.0.join("empty");
    std::fs::create_dir(&empty).unwrap();
    for tolerance in [0.0, 5.0] {
        let view = invoke(
            &webview,
            "scan",
            scan_args(&[empty.to_str().unwrap()], tolerance),
        )
        .unwrap_or_else(|e| panic!("tolerance {tolerance} rejected: {e}"));
        assert_eq!(view["filesScannedText"], "0 files scanned");
    }
}

#[test]
fn file_actions_only_accept_paths_from_the_latest_scan() {
    let (dir, folder) = library();
    let unique = dir.0.join("c.bin").to_str().unwrap().to_string();
    let (_app, webview) = ui();
    let rejected = Err(json!("That file is not part of the latest scan."));
    let (_, paths) = scan(&webview, &folder);
    // Scanned but in no group, so never verified for the UI to act on.
    for path in [&unique, &folder] {
        for cmd in ["open_file", "reveal_file"] {
            assert_eq!(invoke(&webview, cmd, json!({ "path": path })), rejected);
        }
    }
    // A listed file reaches the shell (stubbed in tests) with the right action.
    assert_eq!(
        invoke(&webview, "open_file", json!({ "path": paths[0] })),
        Err(json!(format!(
            "Shell unavailable in tests: reveal=false {}",
            paths[0]
        )))
    );
    assert_eq!(
        invoke(&webview, "reveal_file", json!({ "path": paths[0] })),
        Err(json!(format!(
            "Shell unavailable in tests: reveal=true {}",
            paths[0]
        )))
    );
}

const ONLY_FALLBACK: &str =
    "Permanent deletion is only available for files that could not be trashed.";

#[test]
fn removal_requires_a_scan_and_reports_failures_in_the_shape_the_ui_reads() {
    let (_dir, folder) = library();
    let (_app, webview) = ui();
    assert_eq!(
        invoke(&webview, "trash_files", json!({ "paths": [folder] })),
        Err(json!("Run a scan before removing files"))
    );
    let (_, paths) = scan(&webview, &folder);
    // Every copy selected: validation fails before the trash is tried.
    let result = invoke(
        &webview,
        "trash_files",
        json!({ "paths": paths, "verifyContents": true }),
    )
    .unwrap();
    assert_eq!(keys(&result), ["failures", "summary"]);
    assert_eq!(
        result["summary"]["exactGroups"].as_array().unwrap().len(),
        1
    );
    let failures = result["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2);
    for (failure, path) in failures.iter().zip(&paths) {
        assert_eq!(keys(failure), ["canDeletePermanently", "error", "path"]);
        assert_eq!(failure["path"], json!(path));
        assert_eq!(failure["canDeletePermanently"], false);
        assert!(
            failure["error"]
                .as_str()
                .unwrap()
                .starts_with("Keep at least one")
        );
    }
}

#[test]
fn a_trash_failure_unlocks_permanent_deletion_of_only_that_file() {
    let (_dir, folder) = library();
    let (_app, webview) = ui();
    let (_, paths) = scan(&webview, &folder);
    let delete = |paths: Value| {
        invoke(
            &webview,
            "delete_files_permanently",
            json!({ "paths": paths, "verifyContents": true }),
        )
    };
    assert_eq!(delete(json!([paths[0]])), Err(json!(ONLY_FALLBACK)));

    let result = invoke(&webview, "trash_files", json!({ "paths": [paths[0]] })).unwrap();
    assert_eq!(
        result["failures"],
        json!([{
            "path": paths[0],
            "error": "Trash is unavailable in tests",
            "canDeletePermanently": true,
        }])
    );
    assert_eq!(
        result["summary"]["exactGroups"].as_array().unwrap().len(),
        1
    );
    // The offer names one file; asking for both copies is refused outright.
    assert_eq!(delete(json!(paths)), Err(json!(ONLY_FALLBACK)));

    let result = delete(json!([paths[0]])).unwrap();
    assert_eq!(result["failures"], json!([]));
    assert_eq!(result["summary"]["exactGroups"], json!([]));
    assert_eq!(result["summary"]["reclaimableText"], "0 B reclaimable");
    assert!(!std::path::Path::new(&paths[0]).exists());
    assert_eq!(std::fs::read(&paths[1]).unwrap(), b"duplicate");
    // Each removal replaces the offer, so it cannot be replayed.
    assert_eq!(delete(json!([paths[0]])), Err(json!(ONLY_FALLBACK)));
    assert_eq!(
        invoke(&webview, "open_file", json!({ "path": paths[0] })),
        Err(json!("That file is not part of the latest scan."))
    );
    // The survivor's group dissolved, so nothing vouches for removing it.
    let result = invoke(&webview, "trash_files", json!({ "paths": [paths[1]] })).unwrap();
    assert_eq!(
        result["failures"],
        json!([{
            "path": paths[1],
            "error": "File no longer belongs to a duplicate or comparison group",
            "canDeletePermanently": false,
        }])
    );
}

#[test]
fn a_later_removal_withdraws_an_earlier_permanent_deletion_offer() {
    let (_dir, folder) = library();
    let (_app, webview) = ui();
    let (_, paths) = scan(&webview, &folder);
    invoke(&webview, "trash_files", json!({ "paths": [paths[0]] })).unwrap();
    // Selecting every copy fails validation, which offers no fallback.
    invoke(&webview, "trash_files", json!({ "paths": paths })).unwrap();
    assert_eq!(
        invoke(
            &webview,
            "delete_files_permanently",
            json!({ "paths": [paths[0]] })
        ),
        Err(json!(ONLY_FALLBACK))
    );
}

// Only Windows lets a test rewrite contents while restoring every field
// FileStamp compares; Unix stamps include ctime, which cannot be set back.
#[cfg(windows)]
#[test]
fn content_verification_defaults_off_like_the_ui_checkbox() {
    for verify in [None, Some(true)] {
        let (_dir, folder) = library();
        let (_app, webview) = ui();
        let (_, paths) = scan(&webview, &folder);
        let target = &paths[0];
        let modified = std::fs::metadata(target).unwrap().modified().unwrap();
        std::fs::write(target, b"Duplicate").unwrap();
        std::fs::File::options()
            .write(true)
            .open(target)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let mut args = json!({ "paths": [target] });
        if let Some(verify) = verify {
            args["verifyContents"] = json!(verify);
        }
        let failure = &invoke(&webview, "trash_files", args).unwrap()["failures"][0];
        if verify.is_some() {
            assert_eq!(
                failure["error"],
                "File changed since scanning. Run a new scan before removing it."
            );
            assert!(std::path::Path::new(target).exists());
            continue;
        }
        // Unverified, the edit goes unnoticed: the trash is tried and the
        // fallback deletes without reading contents either.
        assert_eq!(failure["error"], "Trash is unavailable in tests");
        let result = invoke(
            &webview,
            "delete_files_permanently",
            json!({ "paths": [target] }),
        )
        .unwrap();
        assert_eq!(result["failures"], json!([]));
        assert!(!std::path::Path::new(target).exists());
    }
}

#[test]
fn only_one_scan_or_removal_runs_at_a_time() {
    let (_dir, folder) = library();
    let (app, webview) = ui();
    let busy = Err(json!("Another scan or file operation is still running."));
    let guard = app.state::<Operations>().begin().unwrap();
    assert_eq!(invoke(&webview, "scan", scan_args(&[&folder], 1.5)), busy);
    assert_eq!(
        invoke(&webview, "trash_files", json!({ "paths": [folder] })),
        busy
    );
    drop(guard);
    scan(&webview, &folder);
}

#[test]
fn cancel_scan_flags_the_running_operation() {
    let (app, webview) = ui();
    assert_eq!(invoke(&webview, "cancel_scan", json!({})), Ok(Value::Null));
    assert!(app.state::<Operations>().cancelled.load(Ordering::SeqCst));
}

#[test]
fn dropped_paths_keep_only_folders() {
    let (dir, folder) = library();
    let file = dir.0.join("a.bin").to_str().unwrap().to_string();
    let (_app, webview) = ui();
    assert_eq!(
        invoke(
            &webview,
            "folders_from_paths",
            json!({ "paths": [folder, file] })
        ),
        Ok(json!([folder]))
    );
}

#[test]
fn cancelling_stops_a_running_scan_and_discards_the_previous_results() {
    let (_dir, folder) = library();
    let (app, webview) = ui();
    let (_, paths) = scan(&webview, &folder);
    let handle = app.handle().clone();
    // What the cancel button's command does, at the scan's first progress.
    app.listen_any("scan-progress", move |_| {
        handle
            .state::<Operations>()
            .cancelled
            .store(true, Ordering::SeqCst);
    });
    assert_eq!(
        invoke(&webview, "scan", scan_args(&[&folder], 1.5)),
        Err(json!("Scan cancelled."))
    );
    assert_eq!(
        invoke(&webview, "trash_files", json!({ "paths": [paths[0]] })),
        Err(json!("Run a scan before removing files"))
    );
}

#[test]
fn the_final_files_found_count_always_reaches_the_ui() {
    let dir = TestDir::new();
    for i in 0..150 {
        dir.file(&i.to_string(), i.to_string().as_bytes());
    }
    let (app, webview) = ui();
    let counts = Arc::new(Mutex::new(Vec::new()));
    let sink = counts.clone();
    app.listen_any("scan-progress", move |event| {
        let event: Value = serde_json::from_str(event.payload()).unwrap();
        if event["phase"] == "walking" {
            sink.lock()
                .unwrap()
                .push(event["filesFound"].as_u64().unwrap());
        }
    });
    invoke(&webview, "scan", scan_args(&[dir.0.to_str().unwrap()], 1.5)).unwrap();
    // Every hundredth file, then the total, however fast the walk was.
    assert_eq!(*counts.lock().unwrap(), [100, 150]);
}
