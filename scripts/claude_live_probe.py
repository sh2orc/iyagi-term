"""O18 live evidence: one real Claude Code print session (stream-json) plus
a --resume of the recorded session id. Records both transcripts.

Usage: python3 scripts/claude_live_probe.py <dir>
Cost: two ~5-token completions.
"""

import json
import os
import re
import subprocess
import sys

OUTDIR = sys.argv[1] if len(sys.argv) > 1 else "target/live"
os.makedirs(OUTDIR, exist_ok=True)

email = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")


def scrub(text):
    return email.sub("[REDACTED-EMAIL]", text)


def run(args, timeout_s, label):
    env = dict(os.environ)
    shell_args = ' '.join(args)
    proc = subprocess.run(
        shell_args,
        shell=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout_s,
        env=env,
        cwd=os.path.abspath("."),
    )
    stdout = scrub(proc.stdout or "")
    stderr = scrub(proc.stderr or "")
    record = {
        "label": label,
        "argv": args,
        "exit_code": proc.returncode,
        "stdout_lines": stdout.splitlines(),
        "stderr_tail": stderr.splitlines()[-8:],
    }
    return record


def main():
    summary = {}
    # 1) baseline print run
    first = run(
        [
            "claude",
            "-p",
            "Reply with exactly: ok",
            "--output-format",
            "stream-json",
            "--verbose",
        ],
        180,
        "print-baseline",
    )
    summary["first_exit"] = first["exit_code"]

    session_id = None
    result_obj = None
    events = []
    for line in first["stdout_lines"]:
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        events.append(obj)
        if obj.get("type") == "system" and obj.get("subtype") == "init":
            session_id = obj.get("session_id")
        if obj.get("type") == "result":
            result_obj = obj
    summary["session_id_captured"] = bool(session_id)
    summary["event_kinds"] = sorted({o.get("type") for o in events if isinstance(o, dict)})
    summary["result_subtype"] = (result_obj or {}).get("subtype")
    summary["result_text"] = (result_obj or {}).get("result")
    summary["usage_keys"] = sorted(((result_obj or {}).get("usage") or {}).keys())

    # 2) resume the same session
    resumed = None
    if session_id:
        resumed = run(
            [
                "claude",
                "-p",
                "--resume",
                session_id,
                "Reply with exactly: resumed",
                "--output-format",
                "stream-json",
                "--verbose",
            ],
            180,
            "print-resume",
        )
        summary["resume_exit"] = resumed["exit_code"]
        resume_events = []
        for line in resumed["stdout_lines"]:
            try:
                resume_events.append(json.loads(line))
            except json.JSONDecodeError:
                continue
        resume_result = next(
            (o for o in resume_events if isinstance(o, dict) and o.get("type") == "result"), None
        )
        summary["resume_same_session"] = any(
            isinstance(o, dict) and o.get("session_id") == session_id for o in resume_events
        )
        summary["resume_result_text"] = (resume_result or {}).get("result")

    # Persist transcripts
    with open(os.path.join(OUTDIR, "claude_live_transcript.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(
            {
                "version": "claude-code 2.1.263 live",
                "first": first,
                "resumed": resumed,
            },
            f,
            indent=1,
            ensure_ascii=False,
        )
    summary["outcome"] = "ok" if first["exit_code"] == 0 else "failed"
    return summary


print(json.dumps(main(), indent=1, default=str))
