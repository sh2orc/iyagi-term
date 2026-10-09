"""O18 live evidence harness: drives a real `codex app-server` session
(initialize → account/model → thread → turn → interrupt → resume) over
stdio JSONL and records the full transcript, redacting account identifiers.

Usage: python3 scripts/codex_live_probe.py <out-transcript.jsonl>
Cost: one ~10-token completion + one short interrupted turn + one ~10-token
completion on the resumed thread.
"""

import json
import os
import re
import subprocess
import sys
import threading
import time

OUT = sys.argv[1] if len(sys.argv) > 1 else "codex_live_transcript.jsonl"
CWD = os.path.abspath(".")

proc = subprocess.Popen(
    ["codex", "app-server"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    cwd=CWD,
)

messages = []
lock = threading.Lock()
waiters = {}  # id -> Event + slot
next_id = [0]


def fresh_id():
    next_id[0] += 1
    return f"probe-{next_id[0]}"


def reader():
    for raw in proc.stdout:
        line = raw.decode("utf-8", errors="replace").strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        redact(msg)
        with lock:
            messages.append({"dir": "recv", "t": time.time(), "msg": msg})
        mid = msg.get("id")
        if mid in waiters:
            waiters[mid]["slot"].append(msg)
            waiters[mid]["event"].set()
    # EOF


def redact(msg):
    """Remove account identifiers/emails from any string in the message."""
    email = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")

    def walk(node):
        if isinstance(node, dict):
            for key, value in list(node.items()):
                lk = key.lower()
                if isinstance(value, str) and any(
                    t in lk for t in ("account_id", "email", "userid", "user_id")
                ):
                    node[key] = "[REDACTED]"
                else:
                    walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)
        elif isinstance(node, str):
            return

    # email scrub across all strings
    def scrub(node):
        if isinstance(node, dict):
            for key, value in node.items():
                if isinstance(value, str):
                    node[key] = email.sub("[REDACTED-EMAIL]", value)
                else:
                    scrub(value)
        elif isinstance(node, list):
            for item in node:
                scrub(item)

    walk(msg)
    scrub(msg)


def stderr_reader():
    for raw in proc.stderr:
        line = raw.decode("utf-8", errors="replace").strip()
        if line:
            with lock:
                messages.append({"dir": "stderr", "t": time.time(), "line": line[:400]})


threading.Thread(target=reader, daemon=True).start()
threading.Thread(target=stderr_reader, daemon=True).start()


def send(obj):
    redact(obj)
    with lock:
        messages.append({"dir": "send", "t": time.time(), "msg": obj})
    proc.stdin.write((json.dumps(obj) + "\n").encode())
    proc.stdin.flush()


def notify(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": params})


def request(method, params, timeout=90):
    rid = fresh_id()
    entry = {"event": threading.Event(), "slot": []}
    waiters[rid] = entry
    send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    if not entry["event"].wait(timeout):
        waiters.pop(rid, None)
        raise TimeoutError(f"{method} timed out after {timeout}s")
    waiters.pop(rid, None)
    msg = entry["slot"][0]
    if "error" in msg:
        raise RuntimeError(f"{method} error: {msg['error']}")
    return msg.get("result")


def wait_notification(predicate, timeout=120, collect_for=0.0):
    """Wait until a notification matching predicate arrives; then keep
    collecting for collect_for seconds to let related events land."""
    deadline = time.time() + timeout
    index = 0
    while time.time() < deadline:
        with lock:
            snapshot = messages[index:]
        for i, rec in enumerate(snapshot):
            msg = rec.get("msg", {})
            if rec["dir"] == "recv" and msg.get("method") and predicate(msg):
                index += i
                if collect_for:
                    time.sleep(collect_for)
                return msg
        time.sleep(0.15)
    raise TimeoutError("notification wait timed out")


def summarize():
    counts = {}
    with lock:
        for rec in messages:
            msg = rec.get("msg", {})
            if rec["dir"] == "recv" and msg.get("method"):
                counts[msg["method"]] = counts.get(msg["method"], 0) + 1
    return counts


def main():
    result = {}

    # 1) initialize + initialized
    init = request(
        "initialize",
        {
            "clientInfo": {"name": "iyagi-o18-probe", "title": "Iyagi O18 probe", "version": "0.1.0"},
            "capabilities": {},
        },
    )
    result["initialize"] = {"serverInfo": init.get("serverInfo", init)}
    notify("initialized", {})

    # 2) account/read + model/list
    account = request("account/read", {})
    result["account"] = account
    models = request("model/list", {})
    result["model_count"] = len(models.get("models", models if isinstance(models, list) else []))

    # 3) thread/start (read-only sandbox, never-approve, tiny model default)
    thread = request(
        "thread/start",
        {"cwd": CWD, "sandbox": "read-only", "approvalPolicy": "never"},
    )
    inner = thread.get("thread", thread)
    thread_id = (
        thread.get("threadId")
        or inner.get("id")
        or thread.get("id")
        or thread.get("thread_id")
    )
    result["thread_start"] = {"threadId": thread_id, "raw_keys": list(thread.keys())}
    if not thread_id:
        raise RuntimeError(f"no thread id in thread/start result: {thread}")

    # 4) turn/start — plan-like structured result (outputSchema)
    schema = {
        "type": "object",
        "properties": {
            "kind": {"enum": ["report"]},
            "report_text": {"type": "string", "maxLen": 64},
        },
        "required": ["kind", "report_text"],
        "additionalProperties": False,
    }
    turn_res = request(
        "turn/start",
        {
            "threadId": thread_id,
            "input": [{"type": "text", "text": "Reply with exactly: ok"}],
            "outputSchema": schema,
        },
        timeout=180,
    )
    result["turn_start"] = turn_res
    turn1_id = turn_res["turn"]["id"]

    def completed_turn(tid):
        def pred(m):
            if m["method"] != "turn/completed":
                return False
            turn = m.get("params", {}).get("turn", {})
            return turn.get("id") == tid
        return pred

    completed = wait_notification(completed_turn(turn1_id), timeout=180)
    result["turn_completed_status"] = completed.get("params", {}).get("turn", {}).get("status")

    # 5) interrupt: start a longer turn, cancel it quickly
    turn2 = request(
        "turn/start",
        {
            "threadId": thread_id,
            "input": [{"type": "text", "text": "Count slowly from 1 to 100, one number per line."}],
        },
        timeout=60,
    )
    time.sleep(3)
    interrupted = request(
        "turn/interrupt",
        {"threadId": thread_id, "turnId": turn2["turn"]["id"]},
        timeout=30,
    )
    result["interrupt_receipt"] = interrupted
    turn2_id = turn2["turn"]["id"]
    intr_completed = wait_notification(completed_turn(turn2_id), timeout=60)
    result["interrupt_turn_status"] = intr_completed.get("params", {}).get("turn", {}).get("status")

    # 6) resume the SAME thread and run one more tiny turn
    resumed = request("thread/resume", {"threadId": thread_id}, timeout=60)
    result["resume_receipt"] = resumed
    turn3 = request(
        "turn/start",
        {
            "threadId": thread_id,
            "input": [{"type": "text", "text": "Reply with exactly: resumed"}],
        },
        timeout=30,
    )
    final = wait_notification(completed_turn(turn3["turn"]["id"]), timeout=180)
    result["resume_turn_status"] = final.get("params", {}).get("turn", {}).get("status")
    result["resume_final_items"] = [
        item.get("text", item.get("type"))
        for item in final.get("params", {}).get("turn", {}).get("items", [])
    ]

    result["notification_counts"] = summarize()
    result["outcome"] = "ok"
    return result


try:
    summary = main()
except Exception as exc:  # noqa: BLE001 — evidence harness records failures too
    summary = {"outcome": "failed", "error": repr(exc), "notification_counts": summarize()}
finally:
    try:
        proc.terminate()
        proc.wait(timeout=10)
    except Exception:  # noqa: BLE001
        proc.kill()

with open(OUT, "w", encoding="utf-8", newline="\n") as f:
    for rec in messages:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")

summary['model_count_fixed'] = None
print(json.dumps({k: v for k, v in summary.items()}, indent=1, default=str)[:6000])
