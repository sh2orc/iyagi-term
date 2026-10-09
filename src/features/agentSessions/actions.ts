import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import type { DaemonClient } from "../daemon/client";
import type { SessionController } from "../terminal/sessionController";
import { t as translate } from "../../i18n";
import { resumeInfoFrom } from "./types";

export interface AgentSessionActionDeps {
  client: Pick<DaemonClient, "agentSessionForget">;
  controller: Pick<SessionController, "resumeAgentSession" | "focusSessionPane">;
  close: () => void;
  reload: () => void;
  onError: (message: string | null) => void;
}

/**
 * 행 동작 — 컴포넌트와 시험이 같은 경로를 쓴다(이 환경에는 DOM이 없어
 * 클릭을 흉내 낼 수 없으므로, 버튼이 부르는 것을 그대로 노출한다).
 */
export function agentSessionActions(deps: AgentSessionActionDeps) {
  return {
    /** 살아 있는 세션: 그 세션을 보여 주는 pane으로 이동한다. */
    focus(record: AgentSessionRecord): void {
      if (!record.pty_session_id) return;
      deps.controller.focusSessionPane(record.pty_session_id);
      deps.close();
    },
    /**
     * 끝난 세션: 기록된 cwd에서 새 pane으로 이어서 연다.
     *
     * 실행이 끝날 때까지 기다린 뒤에만 닫는다 — 실패하면 대화상자를 열어
     * 둔 채 사유를 보여 준다(컨트롤러가 스스로 토스트를 띄우는 경우도
     * 있지만, 예외가 올라오면 여기서도 알린다).
     */
    async resume(record: AgentSessionRecord): Promise<void> {
      const info = resumeInfoFrom(record);
      if (!info) return;
      try {
        await deps.controller.resumeAgentSession(info, { newPane: true });
      } catch {
        deps.onError(translate("terminal.resume.launchFailed"));
        return;
      }
      deps.onError(null);
      deps.close();
    },
    /** 기록만 지운다(실행 중인 세션에는 손대지 않는다). */
    async forget(record: AgentSessionRecord): Promise<void> {
      try {
        await deps.client.agentSessionForget({ id: record.id });
      } catch {
        deps.onError(translate("agentSessions.forgetFailed"));
        return;
      }
      deps.onError(null);
      deps.reload();
    },
  };
}
