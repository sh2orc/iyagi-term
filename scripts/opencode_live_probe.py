"""O18 live evidence: one real OpenCode session over Z.AI (run + --session
resume + abort semantics). Records the transcript.

Usage: python3 scripts/opencode_live_probe.py <dir>
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
    proc = subprocess.run(
        " ".join(args),
        shell=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout_s,
        cwd=os.path.abspath("."),
    )
    return {
        "label": label,
        "argv": args,
        "exit_code": proc.returncode,
        "stdout": scrub(proc.stdout or ""),
        "stderr_tail": scrub(proc.stderr or "")[-2000:],
    }


def main():
    summary = {}
    # Explicit Z.ai coding-plan provider/model — no implicit default fallback.
    model = "zai-coding-plan/glm-5.3"
    first = run(
        ["opencode", "run", "-m", model, "--print-logs", "--log-level", "ERROR", "Reply with exactly: ok"],
        240,
        "run-first",
    )
    summary["first_exit"] = first["exit_code"]
    session_id = None
    # opencode run prints the assistant text; the session id appears in logs
    # or in the JSON of `opencode` storage. Try scraping stderr/stdout.
    for source in (first["stdout"], first["stderr_tail"]):
        m = re.search(r"(ses_[A-Za-z0-9]+)", source)
        if m:
            session_id = m.group(1)
            break
    summary["session_id_captured"] = bool(session_id)
    summary["first_reply_excerpt"] = first["stdout"].strip()[:200]

    resumed = None
    if session_id:
        resumed = run(
            ["opencode", "run", "-m", model, "--session", session_id, "Reply with exactly: resumed"],
            240,
            "run-resume",
        )
        summary["resume_exit"] = resumed["exit_code"]
        summary["resume_reply_excerpt"] = resumed["stdout"].strip()[:200]

    with open(os.path.join(OUTDIR, "opencode_live_transcript.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(
            {"version": "opencode 1.18.26 live", "model": model, "first": first, "resumed": resumed},
            f,
            indent=1,
            ensure_ascii=False,
        )
    summary["outcome"] = "ok" if first["exit_code"] == 0 else "failed"
    return summary


print(json.dumps(main(), indent=1, default=str))
