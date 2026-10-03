#!/usr/bin/env python3
"""Linux native-window startup smoke check; does not scan or remove any files."""
import os
import signal
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parents[1]
binary = root / "target/debug/duplicate_finder"
process = subprocess.Popen(
    ["dbus-run-session", "--", "xvfb-run", "-a", str(binary)],
    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True,
)
try:
    try:
        stdout, stderr = process.communicate(timeout=5)
        raise SystemExit(f"Desktop app exited during startup ({process.returncode}):\n{stdout}\n{stderr}")
    except subprocess.TimeoutExpired:
        print("PASS native Linux desktop process stayed running for 5 seconds")
finally:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        process.communicate(timeout=10)
