import type { Platform } from "./shortcuts";

const colorCleanupArgs = ["-u", "NO_COLOR", "-u", "FORCE_COLOR", "-u", "CLICOLOR", "-u", "CLICOLOR_FORCE"];

/** Older daemon snapshots and saved layouts can contain our internal launcher. */
export function terminalDisplayTitle(title: string): string {
  const prefix = `env ${colorCleanupArgs.join(" ")} `;
  const normalized = title.startsWith("/usr/bin/env ") ? title.slice("/usr/bin/".length) : title;
  if (!normalized.startsWith(prefix)) return title;
  const program = normalized.slice(prefix.length).split(/\s+-/)[0];
  return program.slice(program.lastIndexOf("/") + 1) || title;
}

/** Also isolates shells launched by an already-running, older daemon. */
export function cleanShellCommand(platform: Platform, shell: { program: string; argv: string[] }): { program: string; argv: string[] } {
  if (platform === "windows") return { program: shell.program, argv: shell.argv };
  return {
    program: "/usr/bin/env",
    argv: [...colorCleanupArgs, shell.program, ...shell.argv],
  };
}
