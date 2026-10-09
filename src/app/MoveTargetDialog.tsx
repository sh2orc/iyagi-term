/**
 * "어느 탭으로?" 대화상자(04-ui.md §2-5) — pane 이동과 탭 합치기가 같은
 * 화면을 쓴다. 고르는 것이 대상 탭 하나로 같고, 막히는 이유(탭당 창 상한)도
 * 같은 규칙이라 화면을 둘로 나눌 이유가 없다.
 *
 * 행을 만드는 규칙은 순수 함수(moveTargets)로 빼 두었다 — 이 환경에는 DOM이
 * 없어 클릭을 흉내 낼 수 없으므로, 목록·비활성 사유 계약은 그 함수로
 * 시험한다(AgentSessionsDialog의 agentSessionActions와 같은 갈래).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { useController } from "./controllerContext";
import { useWorkbenchStore } from "../store/workbenchStore";
import { moveTargets, type MoveTargetModal, type MoveTargetReason } from "./moveTargets";

export function MoveTargetDialog({ modal }: { modal: MoveTargetModal }): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const closeModal = useWorkbenchStore((s) => s.closeModal);
  const tabs = useWorkbenchStore((s) => s.tabs);
  const rows = useMemo(() => moveTargets({ tabs }, modal), [tabs, modal]);
  // 처음 선택은 고를 수 있는 첫 행 — Enter 한 번으로 끝나는 흔한 경우를 위해.
  const [selected, setSelected] = useState(() => Math.max(rows.findIndex((row) => !row.disabled), 0));
  const listRef = useRef<HTMLUListElement>(null);

  // 목록 자체가 초점을 갖는다(팔레트는 입력이 그 역할) — ↑/↓·Enter·Escape가
  // 모두 이 컨테이너에서 처리된다.
  useEffect(() => listRef.current?.focus(), []);

  const clamped = Math.min(selected, Math.max(rows.length - 1, 0));
  const pick = (index: number): void => {
    const row = rows[index];
    if (!row || row.disabled) return;
    // 먼저 닫는다 — 아래 동작은 실패하면 스스로 토스트를 띄운다.
    closeModal();
    if (modal.kind === "move-pane") {
      if (row.kind === "new") controller.detachPaneToNewTab(modal.leafId);
      else if (row.tabId) controller.movePaneToTab(modal.leafId, row.tabId);
      return;
    }
    if (row.tabId) controller.mergeTabs(modal.tabId, row.tabId);
  };

  return (
    <div
      className="move-target-dialog"
      onKeyDown={(event) => {
        if (event.key === "Escape") closeModal();
      }}
    >
      <h2>{t(modal.kind === "move-pane" ? "moveTarget.paneTitle" : "moveTarget.tabTitle")}</h2>
      {rows.length === 0 ? <p className="muted">{t("moveTarget.empty")}</p> : null}
      {rows.length > 0 ? (
        <ul
          ref={listRef}
          className="palette-list move-target-list"
          role="listbox"
          aria-label={t("moveTarget.aria")}
          tabIndex={0}
          aria-activedescendant={rows[clamped] ? `move-target-option-${clamped}` : undefined}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              setSelected(Math.min(clamped + 1, rows.length - 1));
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              setSelected(Math.max(clamped - 1, 0));
            } else if (event.key === "Enter") {
              event.preventDefault();
              pick(clamped);
            }
          }}
        >
          {rows.map((row, index) => (
            <li
              key={row.tabId ?? "new-tab"}
              role="option"
              aria-selected={index === clamped}
              aria-disabled={row.disabled ? true : undefined}
              id={`move-target-option-${index}`}
              className={`move-target-row${row.disabled ? " disabled" : ""}`}
              onMouseEnter={() => setSelected(index)}
            >
              <button
                type="button"
                className={index === clamped ? "palette-selected" : undefined}
                disabled={row.disabled}
                onClick={() => pick(index)}
              >
                <span className="move-target-title">
                  {row.kind === "new" ? t("moveTarget.newTab") : row.title}
                </span>
                {row.paneCount !== null ? (
                  <span className="move-target-count">{t("moveTarget.paneCount", { n: row.paneCount })}</span>
                ) : null}
                {row.reason ? <span className="move-target-reason">{reasonText(row.reason, t)}</span> : null}
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      <div className="modal-actions">
        <button type="button" autoFocus={rows.length === 0} onClick={closeModal}>
          {t("moveTarget.cancel")}
        </button>
      </div>
    </div>
  );
}

function reasonText(reason: MoveTargetReason, t: (key: string, params?: Record<string, string | number>) => string): string {
  return reason.key === "moveTarget.full"
    ? t("moveTarget.full", { max: reason.max })
    : t("moveTarget.tooMany", { n: reason.n, max: reason.max });
}
