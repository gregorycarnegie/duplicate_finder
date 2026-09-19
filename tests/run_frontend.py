#!/usr/bin/env python3
"""Run real-DOM frontend tests in headless Chrome with mocked Tauri IPC."""
import functools
import html
import http.server
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading

root = Path(__file__).resolve().parents[1]
chrome = os.environ.get("CHROME_BIN") or next(
    (path for name in ("google-chrome", "chromium", "chromium-browser") if (path := shutil.which(name))),
    None,
)
if not chrome:
    raise SystemExit("Install Chrome/Chromium or set CHROME_BIN to run frontend tests.")

class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass

with http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(root))) as server:
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="duplicate-finder-chrome-") as profile:
            result = subprocess.run([
                chrome, "--headless", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
                "--disable-background-networking", "--no-first-run", f"--user-data-dir={profile}",
                "--dump-dom", "--virtual-time-budget=15000",
                f"http://127.0.0.1:{server.server_port}/tests/frontend.html",
            ], capture_output=True, text=True, timeout=45)
        match = re.search(r'<pre id="results" data-status="([^"]+)">(.*?)</pre>', result.stdout, re.S)
        if not match:
            raise SystemExit(f"Browser did not produce test results:\n{result.stderr[-3000:]}")
        print(html.unescape(match.group(2)))
        if result.returncode or match.group(1) != "passed":
            raise SystemExit(1)
    finally:
        server.shutdown()
