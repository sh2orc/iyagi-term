import type { IpcAdapter } from "../bridge/ipc";
import { isTauri, tauriIpcAdapter } from "../bridge/ipc";

export interface GitBranchInfo {
  name: string;
  detached: boolean;
  /**
   * 저장소 최상위 작업 디렉터리(`.git`이 있는 폴더 — worktree면 그 worktree
   * 폴더). 재그룹핑의 프로젝트 키(04-ui §2-5). 구 브리지는 이 필드가 없다.
   */
  top_level?: string | null;
}

/** Read-only branch lookup for a terminal cwd. Plain browser previews have no local Git access. */
export async function queryGitBranch(
  cwd: string | null,
  ipc: IpcAdapter = tauriIpcAdapter,
  native = isTauri(),
): Promise<GitBranchInfo | null> {
  if (!cwd || !native) return null;
  try {
    return await ipc.invoke<GitBranchInfo | null>("system_git_branch", { cwd });
  } catch {
    // A deleted cwd, non-repository, or temporarily unavailable Git must not
    // interfere with the terminal itself.
    return null;
  }
}
