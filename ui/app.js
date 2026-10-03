(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const { getCurrentWebview } = window.__TAURI__.webview;
  const { getCurrentWindow } = window.__TAURI__.window;
  const { Menu, MenuItem } = window.__TAURI__.menu;
  const { confirm } = window.__TAURI__.dialog;

  // ---------------------------------------------------------------
  // state
  // ---------------------------------------------------------------

  const state = {
    folders: [],
    summary: null,
    selected: new Set(),
    scanning: false,
    removing: false,
    filesByPath: new Map(),
  };

  // ---------------------------------------------------------------
  // titlebar
  // ---------------------------------------------------------------

  const appWindow = getCurrentWindow();
  const btnMaximize = document.getElementById("btn-maximize");

  async function syncMaximizedState() {
    btnMaximize.classList.toggle("is-maximized", await appWindow.isMaximized());
  }

  document.getElementById("btn-minimize").addEventListener("click", () => appWindow.minimize());
  btnMaximize.addEventListener("click", () => appWindow.toggleMaximize());
  document.getElementById("btn-close").addEventListener("click", () => appWindow.close());

  syncMaximizedState();
  appWindow.onResized(syncMaximizedState);

  // ---------------------------------------------------------------
  // formatting helpers
  // ---------------------------------------------------------------
  // Everything derived from scan results (sizes, durations, dates, group
  // headers) is pre-formatted by the Rust backend. This one survives
  // client-side because it summarizes ephemeral selection state that only
  // ever exists in the webview.

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
    toleranceReadout.textContent = `${Number(toleranceInput.value).toFixed(1)}s maximum spread`;
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

  const btnCancelScan = document.getElementById("btn-cancel-scan");
  btnCancelScan.addEventListener("click", async () => {
    btnCancelScan.disabled = true;
    btnCancelScan.textContent = "Cancelling…";
    try { await invoke("cancel_scan"); }
    catch (err) { showToast(String(err), true); btnCancelScan.disabled = false; }
  });

  async function startScan() {
    if (state.scanning || state.removing) return;
    state.scanning = true;
    state.summary = null;
    state.selected = new Set();
    btnCancelScan.disabled = true;
    btnCancelScan.textContent = "Cancel scan";
    resetScanScreen();
    setScreen("scanning");
    setupNote.textContent = "";

    const options = {
      folders: state.folders,
      durationToleranceSecs: Number(toleranceInput.value),
      minFileSize: Number(minSizeSelect.value),
      includeHidden: includeHiddenInput.checked,
      compareMediaContent: document.getElementById("compare-media").checked,
    };

    const seenFolders = new Set();

    let unlistenProgress = () => {};
    try {
      unlistenProgress = await listen("scan-progress", (event) => {
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
            scanRailFill.style.width = `${Math.min(100, (p.done / p.total) * 40)}%`;
          }
        } else if (p.phase === "hashing") {
          statHashed.textContent = p.done.toLocaleString();
          if (p.done === 1) logLine(`hashing candidates…`);
          if (p.total > 0) {
            scanRailFill.style.width = `${40 + Math.min(40, (p.done / p.total) * 40)}%`;
          }
        } else if (p.phase === "comparing") {
          if (p.done === 0) logLine("comparing sampled media content…");
          if (p.total > 0) scanRailFill.style.width = `${80 + (p.done / p.total) * 10}%`;
        } else if (p.phase === "verifying") {
          if (p.done <= 1) logLine("verifying result files before review…");
          if (p.total > 0) scanRailFill.style.width = `${90 + (p.done / p.total) * 10}%`;
        }
      });
      const pendingScan = invoke("scan", { options });
      btnCancelScan.disabled = false;
      const summary = await pendingScan;
      state.summary = summary;
      state.selected = new Set();
      scanRailFill.style.width = "100%";
      renderResults();
      setScreen("results");
    } catch (err) {
      setScreen("setup");
      setupNote.textContent = String(err);
      if (String(err) !== "Scan cancelled.") showToast(String(err), true);
    } finally {
      state.scanning = false;
      btnCancelScan.disabled = true;
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
  const verifyContentsInput = document.getElementById("verify-contents");

  let contextFile = null;
  let contextCheckbox = null;
  const fileMenu = (async () => {
    const selectItem = await MenuItem.new({
      text: "Select for trash",
      action: () => {
        if (!contextFile || state.removing) return;
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

  function renderFileRow(file) {
    const row = document.createElement("label");
    row.className = "dupe-file";

    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = state.selected.has(file.path);
    checkbox.addEventListener("change", () => {
      if (state.removing) { checkbox.checked = state.selected.has(file.path); return; }
      if (checkbox.checked) state.selected.add(file.path);
      else state.selected.delete(file.path);
      updateLedger();
    });
    row.addEventListener("contextmenu", (event) => showFileMenu(event, file, checkbox));

    const play = document.createElement("button");
    play.type = "button";
    play.className = "dupe-file__play";
    if (file.playable) {
      play.title = "Play";
      play.setAttribute("aria-label", `Play ${file.path}`);
      play.addEventListener("click", (event) => {
        event.preventDefault();
        openFile(file);
      });
    } else {
      play.className += " dupe-file__play--empty";
      play.tabIndex = -1;
      play.setAttribute("aria-hidden", "true");
    }

    const path = document.createElement("span");
    path.className = "dupe-file__path";
    path.textContent = file.path;
    path.title = `Double-click to open ${file.path}`;
    path.addEventListener("dblclick", () => openFile(file));

    const media = document.createElement("span");
    media.className = "dupe-file__media";
    media.textContent = file.detailText;

    const size = document.createElement("span");
    size.className = "dupe-file__size";
    size.textContent = file.sizeText;

    row.append(checkbox, play, path, media, size);
    return row;
  }

  // Keep the initial DOM bounded even when a scan returns thousands of groups
  // or a single group contains many thousands of files.
  function renderPages(container, items, pageSize, render, noun) {
    let offset = 0;
    const more = document.createElement("button");
    more.type = "button";
    more.className = "btn btn--ghost btn--small result-more";
    more.addEventListener("click", appendPage);
    function appendPage() {
      const fragment = document.createDocumentFragment();
      const end = Math.min(offset + pageSize, items.length);
      while (offset < end) fragment.append(render(items[offset++]));
      more.remove();
      container.append(fragment);
      if (offset < items.length) {
        more.textContent = `Show ${Math.min(pageSize, items.length - offset)} more ${noun} (${items.length - offset} remaining)`;
        container.append(more);
      }
    }
    appendPage();
  }

  function renderGroup(group, kind) {
    const card = document.createElement("div");
    card.className = `dupe-group dupe-group--${kind}`;

    const header = document.createElement("div");
    header.className = "dupe-group__header";

    const left = document.createElement("span");
    left.textContent = group.headerLeft;

    const right = document.createElement("span");
    right.className = "dupe-group__reclaim";
    right.textContent = group.headerRight;

    header.append(left, right);
    card.append(header);

    renderPages(card, group.files, 50, renderFileRow, "files");

    return card;
  }

  function renderResults() {
    const s = state.summary;
    state.filesByPath = new Map();
    for (const group of [...s.exactGroups, ...s.mediaGroups]) {
      for (const file of group.files) state.filesByPath.set(file.path, file);
    }
    state.selected = new Set([...state.selected].filter((path) => state.filesByPath.has(path)));
    summaryFiles.textContent = s.filesScannedText;
    summaryReclaim.textContent = s.reclaimableText;
    summaryTime.textContent = s.elapsedText;
    ffmpegNote.hidden = s.ffmpegAvailable;
    const warnings = document.getElementById("scan-warnings");
    warnings.hidden = !s.warnings.length;
    document.getElementById("scan-warning-count").textContent = `${s.warnings.length} scan issue(s) — results may be incomplete`;
    const warningList = document.getElementById("scan-warning-list");
    warningList.replaceChildren();
    renderPages(warningList, s.warnings, 100, (warning) => {
      const li = document.createElement("li");
      li.textContent = warning;
      return li;
    }, "issues");
    resultsEmpty.textContent = s.warnings.length
      ? "No matches found among the files successfully checked. Review the scan issues above."
      : "No matches found.";

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

    renderPages(exactGroupsEl, s.exactGroups, 25, (group) => renderGroup(group, "exact"), "groups");
    renderPages(mediaGroupsEl, s.mediaGroups, 25, (group) => renderGroup(group, "media"), "groups");

    updateLedger();
  }

  function updateLedger() {
    const count = state.selected.size;
    ledger.hidden = count === 0;
    if (count === 0) return;

    let bytes = 0;
    for (const path of state.selected) bytes += state.filesByPath.get(path)?.size || 0;

    ledgerCount.textContent = `${count} selected`;
    ledgerSize.textContent = formatBytes(bytes);
  }

  btnNewScan.addEventListener("click", () => {
    if (state.removing) return;
    document.getElementById("operation-errors").hidden = true;
    state.summary = null;
    state.filesByPath.clear();
    state.selected = new Set();
    updateLedger();
    setScreen("setup");
  });

  btnTrash.addEventListener("click", async () => {
    if (state.removing || state.scanning) return;
    const paths = [...state.selected];
    if (paths.length === 0) return;
    const groups = [...state.summary.exactGroups, ...state.summary.mediaGroups];
    if (groups.some((group) => group.files.every((file) => state.selected.has(file.path)))) {
      showToast("Keep at least one file in each group.", true);
      return;
    }
    const verifyContents = verifyContentsInput.checked;
    state.removing = true;
    verifyContentsInput.disabled = true;
    btnTrash.disabled = true;
    btnNewScan.disabled = true;
    const errorPanel = document.getElementById("operation-errors");
    errorPanel.hidden = true;
    try {
      const noun = paths.length === 1 ? "file" : "files";
      const hasMedia = state.summary.mediaGroups.some((group) => group.files.some((file) => state.selected.has(file.path)));
      const confirmed = await confirm(
        `Move ${paths.length} ${noun} to the trash? This can be undone from your system trash.` +
        (hasMedia ? "\n\nSome selected files are media comparisons, based on duration or sampled content. Unsampled content may be completely different, including video soundtracks. Compare them before removing." : ""),
        { title: "Move to trash", kind: "warning" },
      );
      if (!confirmed) return;
      const result = await invoke("trash_files", { paths, verifyContents });
      state.summary = result.summary;
      let failures = result.failures;
      let failedPaths = new Set(failures.map((f) => f.path));
      state.selected = new Set(paths.filter((p) => failedPaths.has(p)));
      const trashedCount = paths.length - failedPaths.size;
      if (trashedCount > 0) showToast(`Moved ${trashedCount} ${trashedCount === 1 ? "file" : "files"} to trash.`);

      const eligible = failures.filter((f) => f.canDeletePermanently);
      if (eligible.length > 0) {
        const list = eligible.map((f) => `${f.path}: ${f.error}`).join("\n");
        const permanent = await confirm(
          `These files could not be moved to the trash:\n\n${list}\n\nPermanently delete them instead? This cannot be undone.`,
          { title: "Permanently delete", kind: "warning" },
        );
        if (permanent) {
          const permResult = await invoke("delete_files_permanently", { paths: eligible.map((f) => f.path), verifyContents });
          state.summary = permResult.summary;
          failures = [...failures.filter((f) => !f.canDeletePermanently), ...permResult.failures];
          failedPaths = new Set(failures.map((f) => f.path));
          state.selected = new Set(paths.filter((p) => failedPaths.has(p)));
          const deletedCount = eligible.length - permResult.failures.length;
          if (deletedCount > 0) showToast(`Permanently deleted ${deletedCount} ${deletedCount === 1 ? "file" : "files"}.`);
        }
      }
      if (failures.length > 0) {
        errorPanel.textContent = failures.map((f) => `${f.path}: ${f.error}`).join("\n");
        errorPanel.hidden = false;
      }
    } catch (err) {
      errorPanel.textContent = String(err);
      errorPanel.hidden = false;
      showToast(String(err), true);
    } finally {
      state.removing = false;
      verifyContentsInput.disabled = false;
      btnTrash.disabled = false;
      btnNewScan.disabled = false;
      renderResults();
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
