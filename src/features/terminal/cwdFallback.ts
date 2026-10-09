/**
 * 시작 경로를 쓸 수 없을 때(CWD_UNAVAILABLE — 지운 worktree·디렉터리, 빠진
 * 외장 디스크, 권한 없음) 이어서 시도할 상위 경로(04-ui §2-4). 순수 문자열
 * 계산이다 — 실제로 있는지는 데몬이 차례로 확인한다.
 *
 * 파일시스템 뿌리("/", "C:\\", "\\\\server\\share")와 그 바로 아래("/Users",
 * "/Volumes", "C:\\Users")는 상위 후보에서 뺀다: 터미널을 열 자리로는 프로젝트
 * root·home보다 못하다. 뿌리는 모든 후보가 실패했을 때의 마지막 자리로만 쓴다
 * (`filesystemRoot`).
 */

import type { Platform } from "./shortcuts";

interface SplitPath {
  root: string;
  parts: string[];
}

function split(path: string, platform: Platform): SplitPath | null {
  if (platform === "windows") {
    const p = path.replace(/\//g, "\\");
    // 확장 길이·장치 경로(`\\?\C:\…`, `\\?\UNC\server\share\…` — Rust canonicalize가 내는
    // 형태)는 접두를 그대로 둔 채 같은 규칙으로 나눈다. 아래 UNC 규칙에 맡기면 "?"를 서버로
    // 읽어 뿌리가 `\\?\C:`(끝 구분자 없음 — 쓸 수 없는 경로)가 된다.
    const extended = /^\\\\([?.])\\(?:UNC\\([^\\]+)\\([^\\]+)|([A-Za-z]:)(?=\\|$))/i.exec(p);
    if (extended) {
      const [matched, kind, server, share, letter] = extended;
      const root = letter ? `\\\\${kind}\\${letter}\\` : `\\\\${kind}\\UNC\\${server}\\${share}`;
      return { root, parts: p.slice(matched.length).split("\\").filter(Boolean) };
    }
    const drive = /^([A-Za-z]:)\\/.exec(p);
    if (drive) return { root: `${drive[1]}\\`, parts: p.slice(3).split("\\").filter(Boolean) };
    const unc = /^\\\\([^\\]+)\\([^\\]+)/.exec(p);
    if (unc) return { root: `\\\\${unc[1]}\\${unc[2]}`, parts: p.slice(unc[0].length).split("\\").filter(Boolean) };
    return null;
  }
  if (!path.startsWith("/")) return null;
  return { root: "/", parts: path.split("/").filter(Boolean) };
}

function join(root: string, parts: string[], platform: Platform): string {
  if (parts.length === 0) return root;
  const sep = platform === "windows" ? "\\" : "/";
  return root.endsWith(sep) ? root + parts.join(sep) : root + sep + parts.join(sep);
}

/** 가까운 것부터의 상위 경로들(자기 자신·뿌리·뿌리 바로 아래는 빼고). 절대 경로가 아니면 빈 목록. */
export function ancestorDirs(path: string, platform: Platform): string[] {
  const parsed = split(path, platform);
  if (!parsed) return [];
  const out: string[] = [];
  for (let depth = parsed.parts.length - 1; depth >= 2; depth -= 1) {
    out.push(join(parsed.root, parsed.parts.slice(0, depth), platform));
  }
  return out;
}

/** 경로가 놓인 파일시스템 뿌리. 절대 경로가 아니면 null. */
export function filesystemRoot(path: string, platform: Platform): string | null {
  return split(path, platform)?.root ?? null;
}
