"""Independent end-to-end smoke: real iyagi-termd binary + raw wire protocol.

Proves the shipped daemon binary (not the test harness) speaks the contract:
hello auth, snapshot, shell launch through the PTY, request-id dedup, RUNNING
event, graceful shutdown. Exits non-zero on any mismatch.

Input/echo round-trips live in the daemon's own integration suite; this
script stays transport-level and dependency-free (stdlib only).
"""
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

IS_WINDOWS = os.name == "nt"
EXE = ".exe" if IS_WINDOWS else ""
REPO = Path(__file__).resolve().parent.parent
DATA = Path(tempfile.mkdtemp(prefix="iyagi-smoke-"))
FIXTURE = REPO / "target" / "debug" / f"term-fixture{EXE}"
DAEMON = REPO / "target" / "debug" / f"iyagi-termd{EXE}"


class Client:
    def __init__(self, endpoint: str):
        if IS_WINDOWS:
            # Byte-mode named pipe: CreateFile-backed open() works on Windows.
            self.f = open(endpoint, "r+b", buffering=0)
        else:
            # Unix domain socket path (paths.rs main_endpoint on unix).
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.sock.connect(endpoint)
            self.f = self.sock.makefile("rwb", buffering=0)
        self.events: list[dict] = []

    def next_event(self) -> dict:
        if self.events:
            return self.events.pop(0)
        return self.recv()

    def send(self, value: dict):
        body = json.dumps(value).encode()
        self.f.write(struct.pack("<I", len(body)) + body)

    def _read_exact(self, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = self.f.read(n - len(buf))
            if not chunk:
                raise EOFError("pipe closed")
            buf += chunk
        return buf

    def recv(self) -> dict:
        (length,) = struct.unpack("<I", self._read_exact(4))
        return json.loads(self._read_exact(length))

    def rpc(self, id_: str, method: str, params: dict) -> dict:
        self.send({"v": 1, "id": id_, "method": method, "params": params})
        while True:
            msg = self.recv()
            if msg.get("id") == id_:
                return msg
            if "event" in msg:
                self.events.append(msg)


def main() -> int:
    spawn_kwargs = {}
    if IS_WINDOWS:
        # DETACHED_PROCESS: keep the daemon off this console (Windows only).
        spawn_kwargs["creationflags"] = 0x00000008
    daemon = subprocess.Popen(
        [str(DAEMON), "--data-dir", str(DATA)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        **spawn_kwargs,
    )
    try:
        endpoint_file = DATA / "runtime" / "endpoint"
        token_file = DATA / "runtime" / "token"
        for _ in range(100):
            if endpoint_file.exists() and token_file.exists():
                break
            if daemon.poll() is not None:
                print("daemon died:", daemon.stdout.read().decode(errors="replace"))
                return 1
            time.sleep(0.1)
        else:
            print("daemon never became ready")
            return 1
        pipe = endpoint_file.read_text().strip()
        token = token_file.read_text().strip()
        print(f"[smoke] endpoint={pipe}")

        c = Client(pipe)
        hello = c.rpc("1", "hello", {"client_id": str(uuid.uuid4()), "token": token, "role": "control"})
        result = hello.get("result", {})
        assert result.get("protocol") == 1, hello
        assert "capabilities" in result, hello
        print("[smoke] hello ok; capabilities present ✓")

        snap = c.rpc("2", "system.snapshot", {})
        assert "workloads" in snap["result"], snap
        print("[smoke] snapshot revision=", snap["result"]["revision"], "✓")

        req = {
            "request_id": str(uuid.uuid4()),
            "profile_id": str(uuid.uuid4()),
            "cwd": str(DATA),
            "program": str(FIXTURE),
            "argv": ["echo"],
            "env_overrides": {},
            "mode": "shell",
            "cols": 80,
            "rows": 24,
            "priority": 1,
            "policy": {
                "reservation_bytes": "2147483648",
                "cpu_slots": 1,
                "enforcement": "observe",
                "memory_max_bytes": None,
                "cpu_max_cores": None,
                "pids_max": None,
            },
        }
        launch = c.rpc("3", "workload.launch", req)
        res = launch.get("result", {})
        state = res.get("state")
        print(f"[smoke] launch -> state={state} workload={str(res.get('workload_id'))[:8]}")
        assert state in ("RUNNING", "QUEUED", "STARTING"), launch

        dup = c.rpc("4", "workload.launch", req)
        assert dup.get("result", {}).get("workload_id") == res.get("workload_id"), "request-id dedup failed"
        print("[smoke] duplicate request id -> same workload ✓ (01 §6)")

        deadline = time.time() + 20
        saw_running = False
        while time.time() < deadline:
            msg = c.next_event()
            ev, payload = msg.get("event"), msg.get("payload") or {}
            if ev == "workload.changed":
                st = payload.get("state")
                print("[smoke] workload.changed ->", st)
                if st == "RUNNING":
                    saw_running = True
                    break
                if st in ("FAILED", "INTERRUPTED"):
                    print("[smoke] FAIL: terminal state before RUNNING:", payload)
                    return 1
        if not saw_running:
            print("[smoke] FAIL: never reached RUNNING")
            return 1

        c.rpc("6", "daemon.shutdown", {"request_id": str(uuid.uuid4()), "stop_workloads": True})
        rc = daemon.wait(timeout=20)
        print(f"[smoke] daemon exited rc={rc}")
        print("[smoke] PASS")
        return 0 if rc == 0 else 1
    finally:
        if daemon.poll() is None:
            daemon.kill()


if __name__ == "__main__":
    sys.exit(main())
