"""Exercise an installed Codex app-server sandbox without auth or model calls.

Only temporary, probe-owned files and a loopback listener are used. This proves
command/exec enforcement for the recorded CLI/OS, not turn/result/auth support.
Run: python3 scripts/codex_sandbox_probe.py --program /absolute/path/to/codex
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


class Peer:
    def __init__(self, program, root):
        self.sequence = 0
        self.pending = b""
        self.stderr_bytes = 0
        environment = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL") if key in os.environ}
        for key, folder in (("HOME", "home"), ("CODEX_HOME", "codex"), ("TMPDIR", "tmp")):
            directory = root / folder
            directory.mkdir()
            environment[key] = str(directory)
        self.proc = subprocess.Popen(
            [program, "app-server"], cwd=root / "workspace", env=environment,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        )
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ, "stdout")
        self.selector.register(self.proc.stderr, selectors.EVENT_READ, "stderr")

    def send(self, message):
        self.proc.stdin.write(json.dumps(message).encode() + b"\n")
        self.proc.stdin.flush()

    def request(self, method, params):
        self.sequence += 1
        identity = self.sequence
        self.send({"id": identity, "method": method, "params": params})
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            while b"\n" in self.pending:
                line, self.pending = self.pending.split(b"\n", 1)
                message = json.loads(line)
                if message.get("id") == identity:
                    if "error" in message:
                        raise RuntimeError(f"{method}: RPC error {message['error'].get('code')}")
                    return message["result"]
                if "id" in message and "method" in message:
                    raise RuntimeError("unexpected server request; no approvals are granted")
            for key, _ in self.selector.select(min(0.2, max(0, deadline - time.monotonic()))):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    self.selector.unregister(key.fileobj)
                    if key.data == "stdout":
                        raise RuntimeError("app-server closed its protocol stream")
                    continue
                if key.data == "stdout":
                    self.pending += chunk
                    if len(self.pending) > 1024 * 1024:
                        raise RuntimeError("app-server frame exceeded 1 MiB")
                else:
                    self.stderr_bytes += len(chunk)
                    if self.stderr_bytes > 65536:
                        raise RuntimeError("app-server diagnostics exceeded 64 KiB")
        raise TimeoutError(f"{method}: 30-second timeout")

    def close(self):
        # Signal the child-owned group before reaping its root PID.
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(self.proc.pid, signal.SIGKILL)
            self.proc.wait(timeout=3)
        self.selector.close()
        for stream in (self.proc.stdin, self.proc.stdout, self.proc.stderr):
            stream.close()


def policy(workspace, writable):
    if not writable:
        return {"type": "readOnly", "networkAccess": False}
    return {"type": "workspaceWrite", "writableRoots": [str(workspace)],
            "networkAccess": False, "excludeSlashTmp": True, "excludeTmpdirEnvVar": True}


WRITE = """import json, pathlib, sys
try:
    pathlib.Path(sys.argv[1]).write_text('sandbox-probe-mutated')
except OSError as error:
    print(json.dumps({'written': False, 'errno': error.errno}))
else:
    print(json.dumps({'written': True}))
"""


def probe(program):
    if os.name != "posix":
        raise RuntimeError("this probe currently supports macOS/Linux only")
    version = subprocess.run([program, "--version"], capture_output=True, timeout=3, check=True).stdout.decode().strip()
    report = {"format": 1, "runtime": "codex", "version": version,
              "os": platform.system(), "os_release": platform.release(),
              "architecture": platform.machine(), "scope": "command/exec; no model or authentication",
              "tested_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cases": []}
    with tempfile.TemporaryDirectory(prefix="iyagi-codex-sandbox-") as name, tempfile.TemporaryDirectory(prefix="iyagi-codex-slash-tmp-", dir="/tmp") as slash_tmp:
        root = Path(name).resolve()
        workspace = root / "workspace"
        workspace.mkdir()
        outside = root / "outside"
        outside.mkdir()
        for directory in (".git", ".codex", ".agents"):
            (workspace / directory).mkdir()
        (workspace / "external-link").symlink_to(outside, target_is_directory=True)
        peer = Peer(program, root)
        try:
            peer.request("initialize", {"clientInfo": {"name": "iyagi-sandbox-probe", "version": "1"}, "capabilities": {}})
            peer.send({"method": "initialized", "params": {}})
            cases = [
                ("read_only_inside", workspace / "readonly.txt", False, False),
                ("read_only_outside", outside / "readonly.txt", False, False),
                ("workspace_write_inside", workspace / "allowed.txt", True, True),
                ("workspace_write_sibling", outside / "outside.txt", True, False),
                ("workspace_write_symlink", workspace / "external-link" / "linked.txt", True, False),
                ("workspace_write_git", workspace / ".git" / "config", True, False),
                ("workspace_write_codex", workspace / ".codex" / "config.toml", True, False),
                ("workspace_write_agents", workspace / ".agents" / "probe.txt", True, False),
                ("workspace_write_tmpdir", root / "tmp" / "probe.txt", True, False),
                ("workspace_write_slash_tmp", Path(slash_tmp) / "probe.txt", True, False),
            ]
            for label, target, writable, expected in cases:
                target.write_text("sandbox-probe-original")
                before = hashlib.sha256(target.read_bytes()).hexdigest()
                result = peer.request("command/exec", {"command": [sys.executable, "-c", WRITE, str(target)],
                    "cwd": str(workspace), "sandboxPolicy": policy(workspace, writable), "timeoutMs": 5000})
                output = json.loads(result.get("stdout", ""))
                changed = hashlib.sha256(target.read_bytes()).hexdigest() != before
                passed = result.get("exitCode") == 0 and output.get("written") is expected and changed is expected
                report["cases"].append({"case": label, "passed": passed, "written": output.get("written"),
                                        "changed": changed, "errno": output.get("errno")})
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                listener.listen(1)
                command = "import socket,sys; s=socket.create_connection(('127.0.0.1',int(sys.argv[1])),timeout=2); s.close()"
                for writable in (False, True):
                    result = peer.request("command/exec", {"command": [sys.executable, "-c", command, str(listener.getsockname()[1])],
                        "cwd": str(workspace), "sandboxPolicy": policy(workspace, writable), "timeoutMs": 5000})
                    report["cases"].append({"case": "workspace_write_network" if writable else "read_only_network",
                                            "passed": result.get("exitCode") == 1 and "Operation not permitted" in result.get("stderr", "")})
        finally:
            peer.close()
    report["passed"] = all(case["passed"] for case in report["cases"])
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--program", default="codex")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    program = shutil.which(args.program)
    if not program:
        parser.error("Codex executable was not found")
    report = probe(program)
    encoded = json.dumps(report, indent=2) + "\n"
    if args.out:
        args.out.write_text(encoded)
    print(encoded, end="")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
