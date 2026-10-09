/**
 * "프로젝트별로 다시 묶기" 미리보기(04-ui.md §2-5).
 *
 * 배치를 통째로 바꾸는 동작이라 계획을 먼저 보여 준다: 결과 탭이 몇 개인지,
 * 각 탭이 완전히 그대로인지("그대로")·id/이름은 두고 격자만 다시 짜는지
 * ("배치만 정리")·새로 생기는지("새 탭"), 각 탭에 어떤 터미널이 들어가는지.
 * 계획은 대화상자를 연 시점의 것이고, 그사이 배치가 바뀌면 적용이
 * 실패한다(컨트롤러가 사유를 토스트로 알린다).
 */

import { useWorkbenchStore } from "../store/workbenchStore";
import { useController } from "./controllerContext";
import { useI18n } from "../i18n";
import { AgentIcon } from "../features/terminal/AgentIcon";
import { terminalDisplayTitle } from "../features/terminal/shellEnvironment";
import type { RegroupGroup, RegroupPlan } from "../features/terminal/regroup";

/** 그룹 배지 3종: 새 탭 / 배치만 정리(relayout) / 그대로(완전히 손대지 않음). */
function regroupBadgeClass(group: RegroupGroup): "new" | "relayout" | "kept" {
  if (group.keepTabId === null) return "new";
  return group.relayout ? "relayout" : "kept";
}

function regroupBadgeKey(group: RegroupGroup): "regroup.new" | "regroup.relayout" | "regroup.kept" {
  if (group.keepTabId === null) return "regroup.new";
  return group.relayout ? "regroup.relayout" : "regroup.kept";
}

export function RegroupDialog({ plan }: { plan: RegroupPlan }): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  // panes는 스토어가 들고 있는 객체 그대로다(매 렌더 새로 만들지 않는다).
  const panes = useWorkbenchStore((s) => s.panes);

  const apply = () => {
    // 적용 결과(성공·실패)는 컨트롤러가 토스트로 알린다 — 대화상자는 먼저 닫는다.
    closeModal();
    controller.applyRegroup(plan);
  };

  return (
    <div
      className="regroup-dialog"
      onKeyDown={(event) => {
        if (event.key === "Escape") closeModal();
      }}
    >
      <h2>{t("regroup.title")}</h2>
      <p className="muted">{t("regroup.description")}</p>
      {!plan.changed ? (
        // 방어적 경로 — 보통은 컨트롤러가 이 대화상자를 열지 않고 토스트만 띄운다.
        <>
          <p role="status">{t("regroup.noChange")}</p>
          <div className="modal-actions">
            <button type="button" autoFocus onClick={closeModal}>
              {t("notice.confirm")}
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="regroup-summary">
            {t("regroup.summary", { before: plan.tabsBefore, after: plan.tabsAfter })}
          </p>
          <ul className="regroup-list">
            {plan.groups.map((group, index) => (
              <li className="regroup-group" key={group.keepTabId ?? `group-${index}`}>
                <span className="regroup-group-head">
                  <span className="regroup-group-title">{terminalDisplayTitle(group.title)}</span>
                  <span className={`regroup-badge ${regroupBadgeClass(group)}`}>
                    {t(regroupBadgeKey(group))}
                  </span>
                </span>
                <span className="regroup-panes">
                  {group.leafIds.map((leafId) => {
                    const pane = panes[leafId];
                    return (
                      <span className="regroup-pane" key={leafId}>
                        {pane?.agent ? <AgentIcon agent={pane.agent.agent} /> : null}
                        {pane ? terminalDisplayTitle(pane.title) : leafId}
                      </span>
                    );
                  })}
                </span>
              </li>
            ))}
          </ul>
          <div className="modal-actions">
            <button type="button" className="primary" autoFocus onClick={apply}>
              {t("regroup.apply")}
            </button>
            <button type="button" onClick={closeModal}>
              {t("regroup.cancel")}
            </button>
          </div>
        </>
      )}
    </div>
  );
}
