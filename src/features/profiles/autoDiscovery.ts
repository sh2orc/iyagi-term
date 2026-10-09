import type { CliCandidate, SystemProbe } from "./probeTypes";
import { interpreterSuggestionFor } from "./probe";
import { useProfilesStore, type ProfilesStore } from "./profileStore";
import type { CliKind, ProfileInterpreter } from "./types";
import { isWindowsShellShim } from "./validation";

const BUILTIN_KINDS = new Set<CliKind>(["codex", "claude", "opencode"]);

/**
 * Windows의 npm shim(`claude.cmd`·확장자 없는 bash 스크립트)은 관리 실행에서
 * 직접 띄울 수 없다(02 §3) — 같은 종류에 native 후보가 있으면 그쪽을 쓴다.
 */
function isShim(candidate: CliCandidate): boolean {
  return (
    candidate.installForm === "cmd-shim" ||
    candidate.installForm === "script" ||
    isWindowsShellShim(candidate.program)
  );
}

/** 브리지가 `node`를 찾아 줄 수 있으면 쓰는 선택적 확장(없으면 shim은 건너뛴다). */
type ProgramLocator = { locateProgram?: (name: string) => Promise<string | null> };

interface Chosen {
  program: string;
  interpreter: ProfileInterpreter | null;
}

/**
 * 종류별로 등록할 후보를 고른다: native 우선; shim뿐이면 shim이 띄우는
 * `cli.js`를 node interpreter 형태로(브리지가 node 경로를 알려 줄 때만).
 * 둘 다 안 되면 비워 둔다 — 프로필 폼이 interpreter 제안을 보여 준다.
 */
async function chooseCandidate(
  candidates: readonly CliCandidate[],
  locateNode: () => Promise<string | null>,
): Promise<Chosen | null> {
  const native = candidates.find((candidate) => !isShim(candidate));
  if (native) return { program: native.program, interpreter: null };
  for (const shim of candidates) {
    const suggestion = interpreterSuggestionFor(shim, "windows");
    if (!suggestion) continue;
    const node = await locateNode();
    if (!node) return null;
    // program은 shim 그대로 둔다(셸 모드 실행·신원 표시용); 관리 실행은
    // interpreter(node + cli.js)로 간다 — validateProgram이 허용하는 형태.
    return { program: shim.program, interpreter: { executable: node, scriptArgvPrefix: suggestion.scriptArgvPrefix } };
  }
  return null;
}

/** Fill only untouched built-in profiles; user-selected paths always win. */
export async function autoConfigureDetectedProfiles(
  probe: SystemProbe,
  store: ProfilesStore = useProfilesStore,
): Promise<number> {
  const candidates = await probe.listClis();
  const locator = (probe as SystemProbe & ProgramLocator).locateProgram;
  let nodePromise: Promise<string | null> | null = null;
  const locateNode = (): Promise<string | null> => {
    if (!locator) return Promise.resolve(null);
    nodePromise ??= locator("node").catch(() => null);
    return nodePromise;
  };
  const byKind = new Map<string, CliCandidate[]>();
  for (const candidate of candidates) {
    byKind.set(candidate.kind, [...(byKind.get(candidate.kind) ?? []), candidate]);
  }
  const chosenByKind = new Map<string, Chosen>();
  for (const [kind, list] of byKind) {
    const chosen = await chooseCandidate(list, locateNode);
    if (chosen) chosenByKind.set(kind, chosen);
  }

  const targets = store.getState().profiles.filter((profile) =>
    BUILTIN_KINDS.has(profile.descriptor.kind) &&
    profile.label === profile.descriptor.kind &&
    profile.descriptor.program.trim() === "" &&
    chosenByKind.has(profile.descriptor.kind),
  );

  const detected = await Promise.all(targets.map(async (profile) => {
    const chosen = chosenByKind.get(profile.descriptor.kind) as Chosen;
    const version = await probe.queryVersion(chosen.program).catch(() => null);
    return { id: profile.id, ...chosen, version };
  }));

  let applied = 0;
  for (const result of detected) {
    const current = store.getState().profileById(result.id);
    // Re-check after the async version query so a concurrent user edit wins.
    if (!current || current.descriptor.program.trim() !== "") continue;
    const update = store.getState().updateProfile(result.id, {
      descriptor: {
        ...current.descriptor,
        program: result.program,
        detected_version: result.version,
      },
      interpreter: result.interpreter ?? current.interpreter,
    });
    if (update.ok) applied += 1;
  }
  return applied;
}
