/**
 * 설정 페이지(04-ui.md §1: 모달 대신 별도 페이지, 왼쪽 그룹 → 오른쪽 내용,
 * 상단 "터미널로 이동", 그룹 전환은 입력 중인 폼을 유지한다).
 *
 * 구조는 features/settings/schema.ts가 정본이다 — 내비게이션, 설정 검색,
 * 각 행의 라벨이 모두 같은 정의를 읽으므로 그룹/항목이 어긋날 수 없다.
 * 저장 버튼은 없다: 모든 값은 바꾸는 즉시 적용되고 로컬에 저장된다.
 *
 * 패널은 감추기만 하고 unmount하지 않는다(그룹을 오갈 때 관리 실행 폼의
 * 입력이 사라지지 않아야 한다 — 04 §1).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import type { Capabilities } from "../generated/Capabilities";
import type { DaemonClient } from "../features/daemon/client";
import type { Platform } from "../features/terminal/shortcuts";
import { ProfilesPanel } from "../features/profiles/ProfilesPanel";
import { CompatibilityMatrix } from "../features/profiles/CompatibilityMatrix";
import { ManagedRunDialog } from "../features/workloads/ManagedRunDialog";
import { getManagedRunDeps } from "../features/workloads/managedRunDeps";
import { GeneralPanel } from "../features/settings/GeneralPanel";
import { TerminalPanel } from "../features/settings/TerminalPanel";
import { TrackpadPanel } from "../features/settings/TrackpadPanel";
import { ShortcutsPanel } from "../features/settings/ShortcutsPanel";
import { MissionSettings } from "../features/missions/MissionSettings";
import { IntegrationPanel } from "../features/settings/IntegrationPanel";
import {
  groupById,
  searchGroups,
  searchItems,
  type SettingsGroupId,
} from "../features/settings/schema";
import { usePreferences } from "../store/preferences";
import { useShellProfileStore } from "../stores/shellProfileStore";
import { useWorkbenchStore } from "../store/workbenchStore";
import { useI18n, useI18nStore } from "../i18n";
import "./settings.css";

/** 일반·터미널 그룹의 값만 되돌린다 — 실행 프로필과 사용자 셸은 그대로 둔다. */
function resetDeclarativeSettings(): void {
  usePreferences.getState().resetAll();
  useI18nStore.getState().setLanguage(null);
  useShellProfileStore.getState().setDefault(null);
}

export function SettingsPage({ client, platform }: { client: DaemonClient; platform: Platform }): JSX.Element {
  const { t, language } = useI18n();
  // 관리 실행 진입점(팔레트·빈 프로젝트)은 "run" 그룹을 요청한다 — 페이지가
  // 이미 떠 있어도 요청이 바뀌면 그 그룹으로 옮긴다.
  const requestedGroup = useWorkbenchStore((s) => s.settingsGroup);
  const [group, setGroup] = useState<SettingsGroupId>(() => requestedGroup ?? "general");
  const [query, setQuery] = useState("");
  const [highlight, setHighlight] = useState<string | null>(null);
  const [confirmReset, setConfirmReset] = useState(false);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [loadError, setLoadError] = useState(false);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const probe = getManagedRunDeps()?.probe ?? null;
  const returnToTerminal = () => useWorkbenchStore.getState().setPage("terminal");

  // t는 매 렌더 새 클로저다 — 실제 입력은 질의와 언어뿐이라 그 둘로 기억한다.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const visibleGroups = useMemo(() => searchGroups(query, t), [query, language]);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const matchedItems = useMemo(() => searchItems(query, t), [query, language]);

  // 검색으로 현재 그룹이 목록에서 빠지면 첫 결과로 옮겨 준다(빈 화면 방지).
  useEffect(() => {
    if (visibleGroups.length === 0) return;
    if (!visibleGroups.some((entry) => entry.id === group)) setGroup(visibleGroups[0].id);
  }, [visibleGroups, group]);

  useEffect(() => { if (requestedGroup !== null) setGroup(requestedGroup); }, [requestedGroup]);

  useEffect(() => { contentRef.current?.scrollTo(0, 0); }, [group]);

  // 검색 결과를 고르면 그 행으로 스크롤하고 잠깐 강조한다. 강조 클래스는
  // 직접 붙였다 뗀다 — 행의 className은 정적이라 React와 다투지 않는다.
  useEffect(() => {
    if (highlight === null) return;
    const row = contentRef.current?.querySelector(`[data-setting-id="${highlight}"]`);
    if (!row) return;
    row.scrollIntoView?.({ block: "center" });
    row.classList.add("setting-row-highlight");
    const timer = setTimeout(() => setHighlight(null), 1600);
    return () => {
      clearTimeout(timer);
      row.classList.remove("setting-row-highlight");
    };
  }, [highlight, group]);

  useEffect(() => {
    headingRef.current?.focus();
    let active = true;
    void client.systemSnapshot().then(
      snapshot => { if (active) setCapabilities(snapshot.capabilities); },
      () => { if (active) setLoadError(true); },
    );
    return () => { active = false; };
  }, [client]);

  const panel = (id: SettingsGroupId, body: JSX.Element): JSX.Element => (
    <section className="settings-panel" hidden={group !== id} aria-label={t(groupById(id).titleKey)}>
      {body}
    </section>
  );

  return (
    <main className="settings-page" aria-labelledby="settings-title">
      <header className="settings-header">
        <div>
          <h1 id="settings-title" ref={headingRef} tabIndex={-1}>{t("settings.title")}</h1>
          <p>{t("settings.description")} <span className="muted">{t(group === "missions" ? "missions.settings.saveHint" : "settings.applied")}</span></p>
        </div>
        <button type="button" className="settings-return" onClick={returnToTerminal}>{t("settings.return")}</button>
      </header>
      <div className="settings-layout">
        <nav className="settings-nav" aria-label={t("settings.groups")}>
          <input
            type="search"
            className="settings-search"
            value={query}
            placeholder={t("settings.searchPlaceholder")}
            aria-label={t("settings.search")}
            onChange={(event) => setQuery(event.target.value)}
          />
          {visibleGroups.map(entry => (
            <button
              key={entry.id}
              type="button"
              aria-current={group === entry.id ? "page" : undefined}
              onClick={() => setGroup(entry.id)}
            >
              <span>{t(entry.titleKey)}</span>
              <small>{t(entry.hintKey)}</small>
            </button>
          ))}
          {visibleGroups.length === 0 ? <p className="muted settings-search-none" role="status">{t("settings.searchNone")}</p> : null}
          {matchedItems.length > 0 ? (
            <div className="settings-results">
              <h2>{t("settings.searchResults")}</h2>
              <ul>
                {matchedItems.map(entry => (
                  <li key={entry.id}>
                    <button
                      type="button"
                      onClick={() => { setGroup(entry.group); setHighlight(entry.id); }}
                    >
                      <span>{t(entry.labelKey)}</span>
                      <small>{t(groupById(entry.group).titleKey)}</small>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          <div className="settings-nav-footer">
            {confirmReset ? (
              <>
                <p className="muted">{t("settings.resetAllConfirm")}</p>
                <div className="settings-reset-actions">
                  <button type="button" onClick={() => setConfirmReset(false)}>{t("settings.cancel")}</button>
                  <button
                    type="button"
                    onClick={() => { resetDeclarativeSettings(); setConfirmReset(false); }}
                  >
                    {t("settings.resetAllConfirmButton")}
                  </button>
                </div>
              </>
            ) : (
              <button type="button" className="settings-reset-all" onClick={() => setConfirmReset(true)}>
                {t("settings.resetAll")}
              </button>
            )}
          </div>
        </nav>
        <div className="settings-content" ref={contentRef}>
          {panel("general", <GeneralPanel />)}
          {panel("terminal", <TerminalPanel platform={platform} />)}
          {panel("trackpad", <TrackpadPanel />)}
          {panel("run", (
            <ManagedRunDialog client={client} probe={probe} platform={platform} capabilities={capabilities}
              onClose={returnToTerminal} onOpenProfiles={() => setGroup("profiles")} onOpenCompatibility={() => setGroup("compatibility")} />
          ))}
          {panel("profiles", (
            <ProfilesPanel probe={probe} platform={platform === "windows" ? "windows" : "other"} capabilities={capabilities} showCompatibility={false} />
          ))}
          {panel("shortcuts", <ShortcutsPanel platform={platform} />)}
          {panel("integrations", <IntegrationPanel />)}
          {panel("missions", <MissionSettings client={client} />)}
          {panel("compatibility", (
            <>
              <h2>{t("settings.compatibility")}</h2>
              <p className="muted">{t("settings.compatibilityHint")}</p>
              {loadError ? <p role="status">{t("settings.capabilitiesError")}</p> : null}
              <CompatibilityMatrix capabilities={capabilities} />
            </>
          ))}
        </div>
      </div>
    </main>
  );
}
