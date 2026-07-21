(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const { getCurrentWebview } = window.__TAURI__.webview;
  const { Menu, MenuItem } = window.__TAURI__.menu;

  // ---------------------------------------------------------------
  // state
  // ---------------------------------------------------------------

  const state = {
    folders: [],
    summary: null,
    selected: new Set(),
  };

  // ---------------------------------------------------------------
  // formatting helpers
  // ---------------------------------------------------------------

  function formatBytes(n) {
    if (!n || n <= 0) return "0 B";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    const decimals = i === 0 ? 0 : v < 10 ? 1 : 0;
    return `${v.toFixed(decimals)} ${units[i]}`;
  }

  function formatDuration(secs) {
    if (secs == null) return "";
    const totalMs = Math.round(secs * 1000);
    const ms = totalMs % 1000;
    const totalSec = Math.floor(totalMs / 1000);
    const s = totalSec % 60;
    const totalMin = Math.floor(totalSec / 60);
    const m = totalMin % 60;
    const h = Math.floor(totalMin / 60);
    const pad = (n) => String(n).padStart(2, "0");
    const msStr = String(ms).padStart(3, "0");
    return h > 0 ? `${h}:${pad(m)}:${pad(s)}.${msStr}` : `${m}:${pad(s)}.${msStr}`;
  }

  function formatDate(unixSecs) {
    if (unixSecs == null) return "";
    const d = new Date(unixSecs * 1000);
    const now = new Date();
    const opts =
      d.getFullYear() === now.getFullYear()
        ? { month: "short", day: "numeric" }
        : { month: "short", day: "numeric", year: "numeric" };
    return d.toLocaleDateString(undefined, opts);
  }

  function formatMs(ms) {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  }

  // ---------------------------------------------------------------
  // screen switching
  // ---------------------------------------------------------------

  function setScreen(name) {
    document.body.dataset.screen = name;
  }

  function showToast(message, isError = false) {
    const toast = document.getElementById("toast");
    toast.textContent = message;
    toast.classList.toggle("toast--error", isError);
    toast.hidden = false;
    clearTimeout(showToast._t);
    showToast._t = setTimeout(() => {
      toast.hidden = true;
    }, 4000);
  }

  // ---------------------------------------------------------------
  // setup screen
  // ---------------------------------------------------------------

  const sourceList = document.getElementById("source-list");
  const sourceEmpty = document.getElementById("source-empty");
  const btnAddFolder = document.getElementById("btn-add-folder");
  const btnStartScan = document.getElementById("btn-start-scan");
  const toleranceInput = document.getElementById("tolerance");
  const toleranceReadout = document.getElementById("tolerance-readout");
  const minSizeSelect = document.getElementById("min-size");
  const includeHiddenInput = document.getElementById("include-hidden");
  const setupNote = document.getElementById("setup-note");

  function renderSources() {
    sourceList.innerHTML = "";
    sourceEmpty.hidden = state.folders.length > 0;
    for (const folder of state.folders) {
      const li = document.createElement("li");
      li.className = "source-list__row";

      const path = document.createElement("span");
      path.className = "source-list__path";
      path.textContent = folder;
      path.title = folder;

      const remove = document.createElement("button");
      remove.className = "source-list__remove";
      remove.type = "button";
      remove.textContent = "×";
      remove.setAttribute("aria-label", `Remove ${folder}`);
      remove.addEventListener("click", () => {
        state.folders = state.folders.filter((f) => f !== folder);
        renderSources();
      });

      li.append(path, remove);
      sourceList.append(li);
    }
    btnStartScan.disabled = state.folders.length === 0;
  }

  function addFolders(folders) {
    for (const folder of folders) {
      if (!state.folders.includes(folder)) state.folders.push(folder);
    }
    renderSources();
  }

  btnAddFolder.addEventListener("click", async () => {
    try {
      addFolders(await invoke("pick_folders"));
    } catch (err) {
      showToast(String(err), true);
    }
  });

  toleranceInput.addEventListener("input", () => {
    toleranceReadout.textContent = `± ${Number(toleranceInput.value).toFixed(1)}s`;
  });

  btnStartScan.addEventListener("click", startScan);

  // ---------------------------------------------------------------
  // scanning screen
  // ---------------------------------------------------------------

  const statFiles = document.getElementById("stat-files");
  const statHashed = document.getElementById("stat-hashed");
  const statProbed = document.getElementById("stat-probed");
  const scanRailFill = document.getElementById("scan-rail-fill");
  const scanLog = document.getElementById("scan-log");

  function logLine(text) {
    const line = document.createElement("div");
    line.textContent = `> ${text}`;
    scanLog.append(line);
    scanLog.scrollTop = scanLog.scrollHeight;
  }

  function resetScanScreen() {
    statFiles.textContent = "0";
    statHashed.textContent = "0";
    statProbed.textContent = "0";
    scanRailFill.style.width = "0%";
    scanLog.innerHTML = "";
  }

  async function startScan() {
    resetScanScreen();
    setScreen("scanning");
    setupNote.textContent = "";

    const options = {
      folders: state.folders,
      durationToleranceSecs: Number(toleranceInput.value),
      minFileSize: Number(minSizeSelect.value),
      includeHidden: includeHiddenInput.checked,
    };

    const seenFolders = new Set();

    const unlistenProgress = await listen("scan-progress", (event) => {
      const p = event.payload;
      if (p.phase === "walking") {
        statFiles.textContent = p.filesFound.toLocaleString();
        if (!seenFolders.has(p.folder)) {
          seenFolders.add(p.folder);
          logLine(`walking ${p.folder}`);
        }
      } else if (p.phase === "probing") {
        statProbed.textContent = p.done.toLocaleString();
        if (p.done === 1) logLine(`probing media durations…`);
        if (p.total > 0) {
          scanRailFill.style.width = `${Math.min(100, (p.done / p.total) * 50)}%`;
        }
      } else if (p.phase === "hashing") {
        statHashed.textContent = p.done.toLocaleString();
        if (p.done === 1) logLine(`hashing candidates…`);
        if (p.total > 0) {
          scanRailFill.style.width = `${50 + Math.min(100, (p.done / p.total) * 50)}%`;
        }
      }
    });

    try {
      const summary = await invoke("scan", { options });
      state.summary = summary;
      state.selected = new Set();
      scanRailFill.style.width = "100%";
      renderResults();
      setScreen("results");
    } catch (err) {
      setScreen("setup");
      setupNote.textContent = String(err);
      showToast(String(err), true);
    } finally {
      unlistenProgress();
    }
  }

  // ---------------------------------------------------------------
  // results screen
  // ---------------------------------------------------------------

  const summaryFiles = document.getElementById("summary-files");
  const summaryReclaim = document.getElementById("summary-reclaim");
  const summaryTime = document.getElementById("summary-time");
  const ffmpegNote = document.getElementById("ffmpeg-note");
  const exactGroupsEl = document.getElementById("exact-groups");
  const mediaGroupsEl = document.getElementById("media-groups");
  const exactCountEl = document.getElementById("exact-count");
  const mediaCountEl = document.getElementById("media-count");
  const sectionExact = document.getElementById("section-exact");
  const sectionMedia = document.getElementById("section-media");
  const resultsEmpty = document.getElementById("results-empty");
  const btnNewScan = document.getElementById("btn-new-scan");
  const ledger = document.getElementById("ledger");
  const ledgerCount = document.getElementById("ledger-count");
  const ledgerSize = document.getElementById("ledger-size");
  const btnTrash = document.getElementById("btn-trash");

  let contextFile = null;
  let contextCheckbox = null;
  const fileMenu = (async () => {
    const selectItem = await MenuItem.new({
      text: "Select for trash",
      action: () => {
        if (!contextFile) return;
        if (state.selected.has(contextFile.path)) state.selected.delete(contextFile.path);
        else state.selected.add(contextFile.path);
        contextCheckbox.checked = state.selected.has(contextFile.path);
        updateLedger();
      },
    });
    const menu = await Menu.new({
      items: [
        { text: "Open", action: () => openFile(contextFile) },
        { text: "Show in folder", action: () => revealFile(contextFile) },
        selectItem,
      ],
    });
    return { menu, selectItem };
  })();

  function openFile(file) {
    if (file) invoke("open_file", { path: file.path }).catch((err) => showToast(String(err), true));
  }

  function revealFile(file) {
    if (file) invoke("reveal_file", { path: file.path }).catch((err) => showToast(String(err), true));
  }

  async function showFileMenu(event, file, checkbox) {
    event.preventDefault();
    contextFile = file;
    contextCheckbox = checkbox;
    try {
      const { menu, selectItem } = await fileMenu;
      await selectItem.setText(state.selected.has(file.path) ? "Unselect" : "Select for trash");
      await menu.popup();
    } catch (err) {
      showToast(String(err), true);
    }
  }

  function mediaBadge(media) {
    const dims = media.width && media.height ? `${media.width}×${media.height} ` : "";
    const codec = media.codec ? `${media.codec.toUpperCase()} ` : "";
    return `${dims}${codec}${formatDuration(media.durationSecs)}`;
  }

  function renderFileRow(file) {
    const row = document.createElement("label");
    row.className = "dupe-file";

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = state.selected.has(file.path);
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) state.selected.add(file.path);
      else state.selected.delete(file.path);
      updateLedger();
    });
    row.addEventListener("contextmenu", (event) => showFileMenu(event, file, checkbox));

    const path = document.createElement("span");
    path.className = "dupe-file__path";
    path.textContent = file.path;
    path.title = `Double-click to open ${file.path}`;
    path.addEventListener("dblclick", () => openFile(file));

    const media = document.createElement("span");
    media.className = "dupe-file__media";
    media.textContent = file.media ? mediaBadge(file.media) : formatDate(file.modified);

    const size = document.createElement("span");
    size.className = "dupe-file__size";
    size.textContent = formatBytes(file.size);

    row.append(checkbox, path, media, size);
    return row;
  }

  function renderGroup(group, kind) {
    const card = document.createElement("div");
    card.className = `dupe-group dupe-group--${kind}`;

    const header = document.createElement("div");
    header.className = "dupe-group__header";

    const left = document.createElement("span");
    if (kind === "exact") {
      left.textContent = `${group.files.length} files · ${formatBytes(group.files[0].size)} each`;
    } else {
      const durations = group.files.map((file) => file.media.durationSecs);
      const spread = Math.max(...durations) - Math.min(...durations);
      left.textContent = `${group.files.length} files · duration spread ${spread.toFixed(2)}s`;
    }

    const right = document.createElement("span");
    right.className = "dupe-group__reclaim";
    right.textContent = `${formatBytes(group.reclaimableBytes)} reclaimable`;

    header.append(left, right);
    card.append(header);

    for (const file of group.files) {
      card.append(renderFileRow(file));
    }

    return card;
  }

  function renderResults() {
    const s = state.summary;
    summaryFiles.textContent = `${s.filesScanned.toLocaleString()} files scanned`;
    summaryReclaim.textContent = `${formatBytes(s.reclaimableBytes)} reclaimable`;
    summaryTime.textContent = `finished in ${formatMs(s.elapsedMs)}`;
    ffmpegNote.hidden = s.ffmpegAvailable;

    exactGroupsEl.innerHTML = "";
    mediaGroupsEl.innerHTML = "";

    exactCountEl.textContent = s.exactGroups.length
      ? `${s.exactGroups.length} group${s.exactGroups.length === 1 ? "" : "s"}`
      : "";
    mediaCountEl.textContent = s.mediaGroups.length
      ? `${s.mediaGroups.length} group${s.mediaGroups.length === 1 ? "" : "s"}`
      : "";

    sectionExact.hidden = s.exactGroups.length === 0;
    sectionMedia.hidden = s.mediaGroups.length === 0;
    resultsEmpty.hidden = s.exactGroups.length > 0 || s.mediaGroups.length > 0;

    for (const group of s.exactGroups) exactGroupsEl.append(renderGroup(group, "exact"));
    for (const group of s.mediaGroups) mediaGroupsEl.append(renderGroup(group, "media"));

    updateLedger();
  }

  function updateLedger() {
    const count = state.selected.size;
    ledger.hidden = count === 0;
    if (count === 0) return;

    const allFiles = [...state.summary.exactGroups, ...state.summary.mediaGroups].flatMap(
      (g) => g.files,
    );
    const bytes = allFiles.reduce(
      (total, file) => total + (state.selected.has(file.path) ? file.size : 0),
      0,
    );

    ledgerCount.textContent = `${count} selected`;
    ledgerSize.textContent = formatBytes(bytes);
  }

  btnNewScan.addEventListener("click", () => {
    state.summary = null;
    state.selected = new Set();
    updateLedger();
    setScreen("setup");
  });

  function removePathsFromGroups(removed) {
    for (const group of [...state.summary.exactGroups, ...state.summary.mediaGroups]) {
      group.files = group.files.filter((f) => !removed.has(f.path));
    }
    state.summary.exactGroups = state.summary.exactGroups.filter((g) => g.files.length > 1);
    state.summary.mediaGroups = state.summary.mediaGroups.filter((g) => g.files.length > 1);
  }

  btnTrash.addEventListener("click", async () => {
    const paths = [...state.selected];
    if (paths.length === 0) return;
    const noun = paths.length === 1 ? "file" : "files";
    if (!confirm(`Move ${paths.length} ${noun} to the trash? This can be undone from your system trash.`)) {
      return;
    }
    btnTrash.disabled = true;
    try {
      const failures = await invoke("trash_files", { paths });
      const failedPaths = new Set(failures.map((f) => f.path));
      const trashed = new Set(paths.filter((p) => !failedPaths.has(p)));
      removePathsFromGroups(trashed);
      state.selected = new Set([...state.selected].filter((p) => failedPaths.has(p)));

      if (trashed.size > 0) {
        showToast(`Moved ${trashed.size} ${trashed.size === 1 ? "file" : "files"} to trash.`);
      }

      if (failures.length > 0) {
        const failedNoun = failures.length === 1 ? "file" : "files";
        const list = failures.map((f) => f.path).join("\n");
        const permanent = confirm(
          `${failures.length} ${failedNoun} could not be moved to the trash (no recycle bin support, ` +
            `e.g. a network share or NAS):\n\n${list}\n\n` +
            `Permanently delete ${failures.length === 1 ? "it" : "them"} instead? This cannot be undone.`,
        );
        if (permanent) {
          const permFailures = await invoke("delete_files_permanently", { paths: [...failedPaths] });
          const permFailedPaths = new Set(permFailures.map((f) => f.path));
          const deleted = new Set([...failedPaths].filter((p) => !permFailedPaths.has(p)));
          removePathsFromGroups(deleted);
          state.selected = new Set([...state.selected].filter((p) => permFailedPaths.has(p)));
          if (deleted.size > 0) {
            showToast(`Permanently deleted ${deleted.size} ${deleted.size === 1 ? "file" : "files"}.`);
          }
          if (permFailures.length > 0) {
            showToast(`Failed to delete ${permFailures.length} ${permFailures.length === 1 ? "file" : "files"}.`, true);
          }
        }
      }

      renderResults();
    } catch (err) {
      showToast(String(err), true);
    } finally {
      btnTrash.disabled = false;
    }
  });

  // ---------------------------------------------------------------
  // init
  // ---------------------------------------------------------------

  renderSources();
  document.addEventListener("contextmenu", (event) => event.preventDefault());
  getCurrentWebview()
    .onDragDropEvent(async ({ payload }) => {
      if (payload.type !== "drop" || document.body.dataset.screen !== "setup") return;
      try {
        const folders = await invoke("folders_from_paths", { paths: payload.paths });
        addFolders(folders);
        if (folders.length !== payload.paths.length) showToast("Only folders can be added.", true);
      } catch (err) {
        showToast(String(err), true);
      }
    })
    .catch((err) => showToast(String(err), true));
})();
