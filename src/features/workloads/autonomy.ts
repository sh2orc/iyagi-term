import type { CliKind, LaunchProfile } from "../profiles/types";
import { effectiveCommand } from "../profiles/types";
import type { PreferenceValues } from "../../store/preferences";

export function supportsAutonomy(kind: CliKind): kind is "claude" | "codex" {
  return kind === "claude" || kind === "codex";
}

export function autonomyEnabled(kind: CliKind, preferences: PreferenceValues): boolean {
  return kind === "claude" ? preferences.claudeFullAutonomy
    : kind === "codex" ? preferences.codexFullAutonomy : false;
}

const CODEX_SWITCHES = new Set([
  "--dangerously-bypass-approvals-and-sandbox", "--yolo", "--full-auto",
  "--approve-for-me", "--dangerously-bypass-hook-trust",
]);
const CLAUDE_SWITCHES = new Set([
  "--dangerously-skip-permissions", "--allow-dangerously-skip-permissions",
]);
const CODEX_VALUES = new Set(["--ask-for-approval", "-a", "--sandbox", "-s"]);
const CLAUDE_VALUES = new Set(["--permission-mode"]);
// These values are data even when the value happens to look like a CLI flag.
const LITERAL_VALUES = new Set([
  "--model", "-m", "--system-prompt", "--system-prompt-file", "--append-system-prompt",
  "--append-system-prompt-file", "--agent", "--agents", "--settings", "--mcp-config",
  "--session-id", "--output-format", "--input-format", "--json-schema", "--effort",
]);
const PERMISSION_CONFIG = /^(?:approval_policy|sandbox_mode|sandbox_permissions|permissions)(?:\s*=|\.)/;

/** The checkbox owns permission flags; profile/extra args cannot override it.
 * Keep interpreter args and CLI config files untouched. `--` ends option parsing.
 */
export function autonomyArgv(kind: CliKind, argv: readonly string[], enabled = true): string[] {
  if (!supportsAutonomy(kind)) return [...argv];
  const switches = kind === "claude" ? CLAUDE_SWITCHES : CODEX_SWITCHES;
  const values = kind === "claude" ? CLAUDE_VALUES : CODEX_VALUES;
  const cleaned: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--") { cleaned.push(...argv.slice(i)); break; }
    const name = arg.split("=", 1)[0];
    if (switches.has(name)) continue;
    if (values.has(name)) {
      if (arg === name && i + 1 < argv.length && !argv[i + 1].startsWith("-")) i++;
      continue;
    }
    if (kind === "codex" && /^-[as][^-].+/.test(arg)) continue;
    if (kind === "codex" && (arg === "-c" || arg === "--config")) {
      const value = argv[i + 1];
      if (value !== undefined) {
        i++;
        if (!PERMISSION_CONFIG.test(value.trim())) cleaned.push(arg, value);
      } else cleaned.push(arg);
      continue;
    }
    if (kind === "codex" && (arg.startsWith("--config=") || arg.startsWith("-c"))) {
      const value = arg.startsWith("--config=") ? arg.slice(9) : arg.slice(2).replace(/^=/, "");
      if (PERMISSION_CONFIG.test(value.trim())) continue;
    }
    cleaned.push(arg);
    if (LITERAL_VALUES.has(arg) && i + 1 < argv.length) cleaned.push(argv[++i]);
  }
  if (enabled) cleaned.unshift(kind === "claude"
    ? "--dangerously-skip-permissions" : "--dangerously-bypass-approvals-and-sandbox");
  return cleaned;
}

export function effectiveLaunchCommand(
  profile: Pick<LaunchProfile, "descriptor" | "interpreter">,
  extraArgv: readonly string[] = [],
  enabled = true,
): { program: string; argv: string[] } {
  return effectiveCommand({
    ...profile,
    descriptor: {
      ...profile.descriptor,
      argv_prefix: autonomyArgv(profile.descriptor.kind, [...profile.descriptor.argv_prefix, ...extraArgv], enabled),
    },
  });
}
