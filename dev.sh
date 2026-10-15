#!/usr/bin/env python3
"""Dev wrapper for cargo tauri dev. Ctrl+C kills all child processes."""
import subprocess, signal, os, sys, time

# Ensure terminal delivers SIGINT on Ctrl+C (zsh disables this in some configs)
os.system("stty isig 2>/dev/null")

subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "scripts/sdk-prepare.py")], check=True)

proc = subprocess.Popen(
    ["cargo", "tauri", "dev"] + sys.argv[1:],
    start_new_session=True,
)

def kill_all(sig, frame):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    sys.exit(0)

signal.signal(signal.SIGINT, kill_all)
signal.signal(signal.SIGTERM, kill_all)

while proc.poll() is None:
    time.sleep(0.5)
