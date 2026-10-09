import { spawn } from "node:child_process";

// Only these metadata fields cross IPC. Never serialize plugin input, messages,
// config, or environment; OpenCode remains the owner of conversation contents.
export default function reporter() {
  const { IYAGI_DAEMON_BIN: program, IYAGI_DATA_DIR: dataDir,
    IYAGI_SESSION_ID: terminal, IYAGI_WORKLOAD_ID: workload } = process.env;
  let pending = Promise.resolve();
  return (event, id, cwd) => {
    if (!program || !dataDir || !terminal || !workload ||
      typeof id !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(id) ||
      typeof cwd !== "string" || !cwd || cwd.length > 4096 || /[\x00-\x1f]/.test(cwd)) return Promise.resolve();
    const payload = JSON.stringify({ hook_event_name: event, session_id: id, cwd });
    pending = pending.then(() => new Promise(resolve => {
      try {
        const child = spawn(program, ["--data-dir", dataDir, "hook", "--agent", "opencode"], {
          stdio: ["pipe", "ignore", "ignore"], windowsHide: true,
        });
        const timer = setTimeout(() => { child.kill(); resolve(); }, 2500);
        const done = () => { clearTimeout(timer); resolve(); };
        child.on("error", done);
        child.on("close", done);
        child.stdin.on("error", () => {});
        child.stdin.end(payload);
      } catch { resolve(); }
    }));
    return pending;
  };
}
