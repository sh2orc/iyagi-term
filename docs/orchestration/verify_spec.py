"""Offline reference checks, NOT product implementation or provider validation.

Run: python3 docs/orchestration/verify_spec.py
Uses only Python stdlib and an in-memory SQLite database. Never opens user DBs.
"""

import json
from pathlib import Path
import re
import sqlite3
from urllib.parse import unquote, urlsplit

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def read_json(name):
    return json.loads((HERE / name).read_text(encoding="utf-8"))


def verify_defaults(defaults):
    require(defaults["spec_version"] == 1, "spec version")
    for key, value in defaults.items():
        if type(value) is int:
            require(value > 0, f"non-positive default: {key}")
    require(defaults["max_frame_bytes"] == 65536, "existing IPC frame cap changed")
    require(defaults["max_event_bytes"] < defaults["max_frame_bytes"], "event budget")
    require(defaults["snapshot_entity_max_bytes"] < defaults["max_frame_bytes"], "entity frame budget")
    require(defaults["snapshot_control_reserve_bytes"] < defaults["snapshot_max_bytes"], "control reserve")
    require(defaults["max_parallel_runs_per_binding"] <= defaults["max_parallel_runs_per_mission"] <= defaults["max_parallel_runs_global"], "concurrency defaults")
    require(defaults["layout_compact_px"] < defaults["layout_wide_px"], "breakpoints")
    require(defaults["min_lead_width_px"] + defaults["min_team_width_px"] < defaults["layout_wide_px"], "pane widths")
    for key in ("automatic_paid_fallback", "automatic_publish", "automatic_restart_unknown"):
        require(defaults[key] is False, f"unsafe implicit action: {key}")
    ceiling_pairs = {
        "max_parallel_runs": "max_parallel_runs_per_mission",
        "max_attempts_per_task": "max_attempts_per_task",
        "max_repair_cycles": "max_repair_cycles",
        "max_automatic_starts": "max_automatic_mission_starts",
        "active_time_limit_ms": "mission_active_time_limit_ms",
        "run_time_limit_ms": "run_time_limit_ms",
    }
    for field, default_key in ceiling_pairs.items():
        require(defaults["policy_ceiling"][field] >= defaults[default_key], f"policy ceiling {field}")
    payload = {"v": 1, "id": "x" * 36, "method": "artifact.write", "params": {
        "upload_id": "x" * 36, "offset": "9223372036854775807",
        "data_b64": "A" * (((defaults["artifact_chunk_bytes"] + 2) // 3) * 4),
    }}
    require(len(json.dumps(payload).encode()) + 4 < defaults["max_frame_bytes"], "artifact frame too large")


def verify_states(states, cases):
    contract = (HERE / "contracts.ts").read_text(encoding="utf-8")
    names = {"mission": "MissionState", "task": "TaskState", "run": "RunState"}
    for entity, graph in states.items():
        match = re.search(r"export type " + names[entity] + r"\s*=([^;]+);", contract)
        require(match is not None, f"missing state type: {entity}")
        wire = set(re.findall(r"'([^']+)'", match.group(1)))
        require(wire == set(graph), f"wire/state mismatch: {entity}")
        for source, targets in graph.items():
            require(len(targets) == len(set(targets)), f"duplicate edge: {entity}/{source}")
            require(source not in targets, f"self transition: {entity}/{source}")
            require(set(targets) <= wire, f"unknown target: {entity}/{source}")
    for case in cases["transitions"]:
        graph = states[case["entity"]]
        actual = case["to"] in graph[case["from"]]
        require(actual == case["allowed"], f"state case {case['id']}")


def plan_result(tasks):
    ids = [task["id"] for task in tasks]
    if len(set(ids)) != len(ids):
        return "duplicate"
    if len({task["mission"] for task in tasks}) > 1:
        return "cross_mission"
    pending = {task["id"]: set(task["deps"]) for task in tasks}
    for task in tasks:
        if task["id"] in task["deps"]:
            return "self"
        if len(task["deps"]) != len(set(task["deps"])):
            return "duplicate_edge"
        if not set(task["deps"]) <= set(ids):
            return "missing"
    while pending:
        ready = {task_id for task_id, deps in pending.items() if not deps}
        if not ready:
            return "cycle"
        pending = {task_id: deps - ready for task_id, deps in pending.items() if task_id not in ready}
    return "ok"


def accept_result(case):
    integrity_ok = case["integrity"] == "enforced" or (
        case["integrity"] == "observed" and not case["strict"] and case["observed_ack"]
    )
    return all((
        case["current_candidate"] == case["requested_candidate"],
        case["required_tasks_done"], case["verification_passed"], integrity_ok,
        case["review_passed"], case["blocking_findings"] == 0,
        case["human_checks_done"], case["live_or_unknown"] == 0,
        case["open_blocking_decisions"] == 0,
    ))


def verify_cases(cases):
    ids = []
    for group in ("transitions", "plans", "recovery", "acceptance"):
        ids.extend(case["id"] for case in cases[group])
    require(len(ids) == len(set(ids)), "duplicate fixture case ID")
    for case in cases["plans"]:
        require(plan_result(case["tasks"]) == case["expected"], f"plan case {case['id']}")
    for case in cases["recovery"]:
        actual = "inspect" if case["dispatch"] != "unsent" else (
            "dispatch" if case["policy_allows"] and case["mission_running"] else "hold"
        )
        require(actual == case["expected"], f"recovery case {case['id']}")
    for case in cases["acceptance"]:
        require(accept_result(case) == case["expected"], f"acceptance case {case['id']}")
    # Acceptance has independent guards: each false predicate must prevent accept.
    base = cases["acceptance"][0]
    for key in ("required_tasks_done", "verification_passed", "review_passed", "human_checks_done"):
        require(not accept_result({**base, key: False}), f"acceptance guard {key}")
    require(not accept_result({**base, "open_blocking_decisions": 1}), "open decision guard")


def rejected(db, sql, args=()):
    try:
        db.execute(sql, args)
    except sqlite3.IntegrityError:
        return
    raise AssertionError("SQL constraint did not reject: " + sql)


def verify_sql(states, defaults, include_agent_sessions=False):
    db = sqlite3.connect(":memory:")
    try:
        db.executescript((ROOT / "docs/implementation/schema.sql").read_text(encoding="utf-8"))
        if include_agent_sessions:
            db.executescript((ROOT / "docs/implementation/schema-0002-agent-sessions.sql").read_text(encoding="utf-8"))
        db.execute("INSERT INTO tasks VALUES ('legacy', 'keep me', 'time')")
        db.commit()
        ddl = (ROOT / "crates/term-storage/migrations/0003_orchestration.sql").read_text(encoding="utf-8")
        require(ddl.startswith("CREATE TABLE"), "future migration must start with CREATE TABLE")
        require(not re.search(r"\b(DROP|ALTER)\b", ddl, re.I), "reference schema mutates legacy tables")
        # Emulate required atomic DDL + migration version transaction, not current runner.
        try:
            db.executescript("BEGIN IMMEDIATE;\n" + ddl + "\nINSERT INTO missing_table VALUES (1);\nCOMMIT;")
        except sqlite3.OperationalError:
            db.rollback()
        else:
            raise AssertionError("fault injection did not fail")
        require(db.execute("SELECT count(*) FROM sqlite_master WHERE name='orch_missions'").fetchone()[0] == 0, "DDL rollback failed")
        db.executescript("BEGIN IMMEDIATE;\n" + ddl + "\nCOMMIT;")
        require(db.execute("SELECT title FROM tasks WHERE id='legacy'").fetchone()[0] == "keep me", "legacy row changed")
        for index, state in enumerate(states["mission"]):
            db.execute("INSERT INTO orch_missions VALUES (?,1,1,?,'{}','time','time',NULL)", (f"m{index}", state))
        rejected(db, "INSERT INTO orch_missions VALUES ('bad',1,1,'bogus','{}','t','t',NULL)")
        rejected(db, "INSERT INTO orch_missions VALUES ('bad',1,2,'draft','{}','t','t',NULL)")
        for index, state in enumerate(states["task"]):
            db.execute("INSERT INTO orch_tasks VALUES (?,'m0',?,?,0,'{}')", (f"t{index}", state, index))
        db.execute("INSERT INTO orch_tasks VALUES ('foreign','m1','ready',0,0,'{}')")
        rejected(db, "INSERT INTO orch_dependencies VALUES ('m0','t0','foreign')")
        rejected(db, "INSERT INTO orch_dependencies VALUES ('m0','t0','t0')")
        db.execute("INSERT INTO orch_dependencies VALUES ('m0','t1','t0')")
        rejected(db, "INSERT INTO orch_dependencies VALUES ('m0','t1','t0')")
        run_sql = "INSERT INTO orch_runs VALUES (?,'m0',?,?,?,1,'unsent','{}')"
        for index, state in enumerate(states["run"]):
            # one distinct task for each state, plus a duplicate live-run check below
            task_id = f"run-task-{index}"
            db.execute("INSERT INTO orch_tasks VALUES (?,'m0','running',?,0,'{}')", (task_id, 100 + index))
            db.execute(run_sql, (f"r{index}", task_id, 1, state))
        rejected(db, run_sql, ("duplicate-live", "run-task-0", 2, "starting"))
        rejected(db, run_sql, ("bad-enum", "t0", 1, "bogus"))
        rejected(db, run_sql, ("bad-ref", "foreign", 1, "prepared"))
        db.execute("INSERT INTO orch_requests VALUES ('req','m0','mission.control',?,'{}','time')", ("a" * 64,))
        rejected(db, "INSERT INTO orch_requests VALUES ('req','m0','mission.control',?,'{}','time')", ("b" * 64,))
        rejected(db, "INSERT INTO orch_events VALUES ('m0',1,2,'tx','changed','{}','time')")
        artifact_sql = "INSERT INTO orch_artifacts VALUES (?,NULL,'client',?,?, 'text/plain',?,'available',0,'time')"
        db.execute(artifact_sql, ("artifact", "a" * 64, 0, "artifacts/a"))
        rejected(db, artifact_sql, ("too-large", "b" * 64, defaults["max_artifact_bytes"] + 1, "artifacts/b"))
        require(db.execute("PRAGMA integrity_check").fetchone()[0] == "ok", "SQLite integrity")
        require(db.execute("PRAGMA foreign_key_check").fetchall() == [], "SQLite foreign keys")
    finally:
        db.close()


def verify_docs():
    documents = [ROOT / "ORCHESTRATION_SPEC.md", *sorted(HERE.glob("*.md"))]
    links = 0
    for document in documents:
        content = document.read_text(encoding="utf-8")
        require(content.count("```") % 2 == 0, f"unclosed code fence: {document.name}")
        for target in re.findall(r"\[[^\]]+\]\(([^)]+)\)", content):
            target = target.strip("<>")
            parsed = urlsplit(target)
            if parsed.scheme or not parsed.path:
                continue
            path = (document.parent / unquote(parsed.path)).resolve()
            require(path.is_relative_to(ROOT), f"link escapes repository: {target}")
            require(path.exists(), f"broken link in {document.name}: {target}")
            links += 1
    for path in [ROOT / "ORCHESTRATION_SPEC.md", *HERE.iterdir()]:
        if path.is_file():
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                require(line == line.rstrip(), f"trailing whitespace: {path.name}:{number}")
    tickets = (HERE / "07-tickets.md").read_text(encoding="utf-8")
    for number in range(1, 22):
        require(f"## O{number:02d}." in tickets, f"missing O{number:02d}")
    verification = (HERE / "06-verification.md").read_text(encoding="utf-8")
    for prefix, count in (("E", 30), ("W", 12), ("U", 18)):
        for number in range(1, count + 1):
            require(f"| {prefix}{number:02d} |" in verification, f"missing {prefix}{number:02d}")
    return len(documents), links


def main():
    defaults, states, cases = (read_json(name) for name in ("defaults.json", "states.json", "cases.json"))
    verify_defaults(defaults)
    verify_states(states, cases)
    verify_cases(cases)
    verify_sql(states, defaults)
    has_agent_sessions = (ROOT / "docs/implementation/schema-0002-agent-sessions.sql").is_file()
    if has_agent_sessions:
        verify_sql(states, defaults, include_agent_sessions=True)
    document_count, links = verify_docs()
    case_count = sum(len(cases[key]) for key in ("transitions", "plans", "recovery", "acceptance"))
    print(f"O1 spec OK: {document_count} documents, {links} local links, {case_count} reference cases; SQL constraints/rollback OK.")
    print("Baseline 0001 checked; baseline 0002 " + ("also checked." if has_agent_sessions else "not present in this checkout."))
    print("Specification checks only; application, GUI, and provider compatibility are not validated.")


if __name__ == "__main__":
    main()
