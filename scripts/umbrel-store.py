#!/usr/bin/env python3
"""Serve this repo's Umbrel app store as a community app store to a local umbrelOS VM.

umbrelOS fetches app stores with isomorphic-git, which only speaks Git's smart HTTP
protocol. This wraps `git http-backend` and, on every fetch, snapshots the working tree
copy of the store (umbrel-app-store.yml and allod/) into a scratch repo, so uncommitted
edits reach the VM without a push.

    just umbrel-store [192.168.122.1:8765]

Add http://<listen>/allod-store.git in umbrelOS: App Store > ... > Community App Stores.
Dev only: no auth. Bind it to the VM bridge, not a public interface.
"""

import argparse
import http.server
import os
import shutil
import subprocess
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / "deploy" / ".dev" / "umbrel-store"
REPO = WORK / "allod-store.git"
TREE = WORK / "tree"
BACKEND = subprocess.run(
    ["git", "--exec-path"], capture_output=True, text=True, check=True
).stdout.strip() + "/git-http-backend"

sync_lock = threading.Lock()


def git(*args, cwd=TREE):
    env = dict(os.environ, GIT_DIR=str(REPO), GIT_WORK_TREE=str(TREE))
    env.update(GIT_AUTHOR_NAME="allod-dev", GIT_AUTHOR_EMAIL="dev@allod.invalid")
    env.update(GIT_COMMITTER_NAME="allod-dev", GIT_COMMITTER_EMAIL="dev@allod.invalid")
    return subprocess.run(["git", *args], cwd=cwd, env=env, check=True,
                          capture_output=True, text=True).stdout


def sync():
    """Mirror the store into the scratch repo and commit any change."""
    with sync_lock:
        if not REPO.exists():
            subprocess.run(["git", "init", "-q", "--bare", "-b", "master", str(REPO)], check=True)
            (REPO / "git-daemon-export-ok").touch()
        if TREE.exists():
            shutil.rmtree(TREE)
        TREE.mkdir(parents=True)
        shutil.copy2(ROOT / "umbrel-app-store.yml", TREE)
        shutil.copytree(ROOT / "allod", TREE / "allod")
        git("add", "-A")
        if git("status", "--porcelain"):
            git("commit", "-q", "-m", "working tree snapshot")
            print(f"store updated: {git('rev-parse', '--short', 'HEAD').strip()}", flush=True)


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.backend()

    def do_POST(self):
        self.backend()

    def backend(self):
        path, _, query = self.path.partition("?")
        if not path.startswith("/allod-store.git/"):
            self.send_error(404)
            return
        if path.endswith("/info/refs"):
            try:
                sync()
            except subprocess.CalledProcessError as e:
                print(f"sync failed: {e.stderr}", file=sys.stderr, flush=True)
                self.send_error(500)
                return
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        env = {
            "GIT_PROJECT_ROOT": str(WORK),
            "GIT_HTTP_EXPORT_ALL": "1",
            "REQUEST_METHOD": self.command,
            "PATH_INFO": path,
            "QUERY_STRING": query,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(len(body)),
            "HTTP_CONTENT_ENCODING": self.headers.get("Content-Encoding", ""),
            "HTTP_GIT_PROTOCOL": self.headers.get("Git-Protocol", ""),
            "REMOTE_ADDR": self.client_address[0],
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        out = subprocess.run([BACKEND], input=body, env=env, capture_output=True).stdout
        head, _, payload = out.partition(b"\r\n\r\n")
        status = 200
        headers = []
        for line in head.split(b"\r\n"):
            key, _, value = line.decode().partition(":")
            if key.lower() == "status":
                status = int(value.strip().split()[0])
            elif key:
                headers.append((key, value.strip()))
        self.send_response(status)
        for key, value in headers:
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--listen", default="192.168.122.1:8765")
    args = parser.parse_args()
    host, _, port = args.listen.rpartition(":")
    WORK.mkdir(parents=True, exist_ok=True)
    sync()
    server = http.server.ThreadingHTTPServer((host, int(port)), Handler)
    print(f"serving http://{args.listen}/allod-store.git", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
