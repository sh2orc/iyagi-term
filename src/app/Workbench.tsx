/**
 * Workbench (04-ui.md §1): 상단(탭·새 터미널·관리 실행),
 * 중앙 분할 터미널, 하단 28px resource strip, 우측 drawer(기본 닫힘).
 *
 * 전역 keydown: 앱 단축키에 일치할 때만 preventDefault(04 §3).
 * IME composition 상태를 중앙 key handler에 전달한다(04 §3).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import type { DaemonClient } from "../features/daemon/client";
import { SessionController } from "../features/terminal/sessionController";
import { TerminalRegistry } from "../features/terminal/registry";
import {
  browserResizeObserverFactory,
  attachWebKitImeBridge,
  createDefaultFitAddon,
  createDefaultTerminalFactory,
  createWebglRenderer,
  getSearchAddon,
  serializeTerminalState,
  setOsc52Enabled,
  terminalTheme,
} from "../features/terminal/xtermSetup";
import { createIndexedDbSnapshotStore } from "../features/terminal/replaySnapshot";
import { resolveTheme, startThemeSync, systemPrefersLight, type ResolvedTheme } from "./theme";
import { usePreferences } from "../store/preferences";
import { currentPlatform, detectPlatform, mapShortcut, shortcutHint, type Platform, type ShortcutContext } from "../features/terminal/shortcuts";
import { isNativePasteKey, terminalLeafIdOf } from "../features/terminal/nativePaste";
import { attachTabSwipe } from "../features/terminal/tabSwipeDom";
import { tabSwitchDirection } from "../features/terminal/tabSwitchEffect";
import { imageFileOf, pastedText } from "../features/terminal/clipboardImage";
import { leafIdAtPoint, markDropTarget, toCssPoint } from "../features/terminal/dropPaste";
import { tauriPasteImagePort } from "../features/terminal/pasteImage";
import { listenFileDrop } from "../features/app/fileDrop";
import { useWorkbenchStore } from "../store/workbenchStore";
import { ControllerContext, useController } from "./controllerContext";
import { SplitContainer } from "../features/terminal/SplitContainer";
import { ResourceStrip } from "../features/monitor/ResourceStrip";
import { GraphDrawer } from "../features/monitor/GraphDrawer";
import { QueueDrawer } from "../features/workloads/QueueDrawer";
import { ModalHost } from "./Modals";
import { isTauri } from "../features/bridge/ipc";
import { installNativeMenu } from "./nativeMenu";
import { requestCloseTab } from "./tabCommands";
import { LiveTabItem } from "./TabItem";
import { LayoutDragLayer } from "./LayoutDragLayer";
import { LayoutEditorPage } from "./LayoutEditorPage";
import { layoutPageAllowsAction, leavesLayoutPage } from "./layoutPage";
import { startLayoutDrag } from "./layoutDragSession";
import { SettingsPage } from "./SettingsPage";
import { ShellPicker } from "../features/terminal/ShellPicker";
import { useShellProfileStore } from "../stores/shellProfileStore";
import { CMD_PATH, profileDisplayLabel, resolveProfile } from "../features/terminal/shellProfiles";
import { cachedBuiltinProfiles, detectShellProfiles } from "../features/terminal/shellDeps";
import { t, useI18n, useI18nStore } from "../i18n";
import type { Language } from "../i18n";
import { getManagedRunDeps } from "../features/workloads/managedRunDeps";
import { getLastQuickstart, rememberQuickstart } from "../features/workloads/quickstartMemory";
import { candidatesForKind, discoverClis } from "../features/profiles/probe";
import { isWindowsShellShim } from "../features/profiles/validation";
import type { CliCandidate, CliCandidateKind } from "../features/profiles/probeTypes";
import { terminalDisplayTitle } from "../features/terminal/shellEnvironment";
import { agentModelLabel } from "../features/terminal/agentModel";
import { resolveAgentActivity } from "../features/terminal/agentNames";
import { useTerminalActivity } from "../features/terminal/activity";
import { NotificationBell } from "../features/notifications/NotificationBell";
import { autonomyArgv, autonomyEnabled } from "../features/workloads/autonomy";
import { CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY, claudeProviderFor } from "../features/workloads/claudeProvider";
import { AutonomyToggle } from "../features/workloads/AutonomyToggle";
import { listenQuitRequested, tauriQuitPort } from "../features/app/quit";
import { startMissionSync } from "../features/missions/store";
import { setMissionClient } from "../features/missions/clientAccess";
import { MissionCreateSidebar } from "../features/missions/MissionCreateSidebar";
import { MissionPage } from "../features/missions/MissionPage";
import { MissionAgentView } from "../features/missions/MissionAgentView";
import { openMissionList, useMissionEntryState } from "../features/missions/entryPoints";
import { focusedRepositoryHint, openMissionCreate } from "../features/missions/entry";
import "./workbench.css";

export interface WorkbenchProps {
  client: DaemonClient;
  platform?: Platform;
  projectRoot?: string | null;
  home?: string;
}

/**
 * 재개용 CLI 탐지 결과 캐시(04-ui §5): 기록에 실행 경로가 없는 세션을
 * 이어서 열 때마다 PATH를 다시 훑지 않는다. 성공한 탐지는 앱 수명 동안
 * 그대로 쓰고, 실패·빈 결과는 기억하지 않아 다음 시도에서 다시 묻는다
 * (앱을 켠 뒤 CLI를 설치한 경우).
 */
let agentCliDiscovery: ReturnType<typeof discoverClis> | null = null;

function discoverAgentClisOnce(
  probe: NonNullable<Parameters<typeof discoverClis>[0]>,
): ReturnType<typeof discoverClis> {
  if (!agentCliDiscovery) {
    agentCliDiscovery = discoverClis(probe).then((state) => {
      if (state.status !== "ready" || state.candidates.length === 0) agentCliDiscovery = null;
      return state;
    });
  }
  return agentCliDiscovery;
}

export function Workbench(props: WorkbenchProps): JSX.Element {
  // i18n.t는 렌더링용(구독), 모듈 t는 useMemo 클로저 등 비반응 경로용
  // (클로저가 언어 전환 전 t를 물지 않는다).
  const i18n = useI18n();
  const platform = useMemo(
    () => props.platform ?? detectPlatform(navigator.userAgent, navigator.platform),
    [props.platform],
  );

  const imeRef = useRef(false);
  // 트랙패드 제스처를 붙일 워크벤치 루트(설정·배치 편집 화면에서는 떼어 둔다).
  const workbenchRef = useRef<HTMLDivElement>(null);
  // 탭 전환 효과(features/terminal/tabSwitchEffect.ts): 들어오는 탭이 진행
  // 방향에서 살짝 밀려 들어온다. 애니메이션은 .split-scroll에 걸고, 직전 탭
  // 인덱스로 방향을 정한다.
  const splitScrollRef = useRef<HTMLDivElement>(null);
  const prevTabIndexRef = useRef<number | null>(null);
  // xterm이 앱 단축키 조합을 소비하지 않게 하는 필터(design §10).
  // ref로 전달해 registry/factory 신원을 유지한다.
  const consumeKeyRef = useRef<(event: KeyboardEvent) => boolean>(() => true);
  consumeKeyRef.current = (event: KeyboardEvent) => {
    const store = useWorkbenchStore.getState();
    return (
      mapShortcut(
        {
          key: event.key,
          code: event.code,
          ctrlKey: event.ctrlKey,
          metaKey: event.metaKey,
          altKey: event.altKey,
          shiftKey: event.shiftKey,
          repeat: event.repeat,
        },
        {
          platform,
          imeComposing: imeRef.current || event.isComposing === true,
          modalOpen: store.modal !== null || store.page === "settings",
          hasSelection: false,
          overrides: usePreferences.getState().shortcutOverrides,
        },
      ) === null
    );
  };

  const controller = useMemo(
    () =>
      new SessionController({
        client: props.client,
        resolveShell: () => {
          const state = useShellProfileStore.getState();
          // 캐시된 탐지 결과 우선; 없으면 플랫폼 기본(비동기 탐지는
          // ShellPicker 마운트 시 채워진다).
          const cached = resolveProfile(
            [...cachedBuiltinProfiles(platform), ...state.custom],
            state.defaultProfileId,
          );
          return cached
            ? { program: cached.program, argv: cached.argv, label: profileDisplayLabel(cached, t) }
            : {
                // 탐지 결과가 아직 없을 때의 최후 폴백 — 플랫폼별 항상 있는 셸.
                program: platform === "windows" ? CMD_PATH : "/bin/sh",
                argv: [],
                label: t("terminal.fallbackShell"),
              };
        },
        registry: new TerminalRegistry({
          createTerminal: createDefaultTerminalFactory({
            consumeKey: (event) => consumeKeyRef.current(event),
          }),
          createFitAddon: createDefaultFitAddon,
          createResizeObserver: browserResizeObserverFactory,
          // WebGL 렌더러(설정 `gpuRenderer`, 기본 켬): 보이는 pane에만 붙는다.
          createRenderer: createWebglRenderer,
          rendererEnabled: usePreferences.getState().gpuRenderer,
          // WebKit(WKWebView)의 조합 이벤트 없는 IME 전달을 xterm에 맞게
          // 조정한다(imeBridge.ts — Chromium 계열에선 자동 무장 해제).
          // 반환된 해제 함수는 registry가 xterm 폐기 직전에 부른다.
          onOpen: (terminal, dom) => attachWebKitImeBridge(dom as HTMLElement, terminal),
        }),
        platform,
        // 에이전트 세션 재개(04-ui §5): 기록에 절대 경로가 없을 때만 PATH
        // 탐지 결과에서 같은 종류의 CLI를 고른다. 없으면 null — 컨트롤러가
        // 문구만 띄우고 아무것도 실행하지 않는다. 탐지는 한 번만 한다
        // (discoverAgentClisOnce — 재개마다 PATH를 다시 훑지 않는다).
        resolveAgentProgram: async (kind) => {
          const probe = getManagedRunDeps()?.probe ?? null;
          if (!probe) return null;
          const state = await discoverAgentClisOnce(probe);
          if (state.status !== "ready") return null;
          const candidate = candidatesForKind(state.candidates, kind).find(
            (c) => !(platform === "windows" && isWindowsShellShim(c.program)),
          );
          return candidate?.program ?? null;
        },
        config: { projectRoot: props.projectRoot ?? null, home: props.home },
        quit: isTauri() ? tauriQuitPort() : undefined,
        savePasteImage: isTauri() ? tauriPasteImagePort() : undefined,
        searchHandler: (terminal, query, direction) => {
          const addon = getSearchAddon(terminal);
          if (!addon) return false;
          return direction === "next" ? addon.findNext(query) : addon.findPrevious(query);
        },
        // 화면 스냅샷: 앱을 다시 켤 때 저널 전체 대신 스냅샷 + 그 뒤 레코드만 재생한다.
        replaySnapshots: typeof indexedDB !== "undefined" ? createIndexedDbSnapshotStore() : undefined,
        serializeTerminal: serializeTerminalState,
      }),
    [props.client, props.projectRoot, props.home, platform],
  );

  /**
   * 첫 터미널: 설정에 지정된 기본 프로필이면 그 셸로 전체 창 pane 1개를
   * 바로 띄우고, 미지정이면 shell-select 대화상자로 물어본다.
   */
  const startFirstTerminal = () => {
    if (useWorkbenchStore.getState().tabs.length !== 0) return;
    const shellState = useShellProfileStore.getState();
    const resolved = shellState.defaultProfileId
      ? resolveProfile(
          [...cachedBuiltinProfiles(platform), ...shellState.custom],
          shellState.defaultProfileId,
        )
      : null;
    if (resolved) {
      controller.newTerminal({
        program: resolved.program,
        argv: resolved.argv,
        label: profileDisplayLabel(resolved, t),
      });
    } else {
      useWorkbenchStore.getState().openModal({ kind: "shell-select" });
    }
  };

  // macOS 메뉴 막대(04-ui §3-2): 앱 기능 전체를 싣고 상태·언어·단축키 재정의·셸 목록을 따라 고친다.
  // 종료는 앱의 확인 흐름을, 정보는 앱 대화상자를 연다.
  useEffect(() => {
    if (!isTauri()) return;
    return installNativeMenu({
      platform,
      controller,
      onAbout: (version) => {
        const store = useWorkbenchStore.getState();
        if (store.modal?.kind !== "quit") store.openModal({ kind: "about", version });
      },
      onError: () => useWorkbenchStore.setState({ toast: t("app.nativeMenuFailed") }),
    });
  }, [platform, controller]);

  // 사용자 홈 → 경로 표시(`~`) 전용. 컨트롤러의 대체 홈은 넣지 않는다.
  useEffect(() => {
    useWorkbenchStore.getState().setHomeDir(props.home ?? null);
  }, [props.home]);

  // mission.changed hint 구독(05 §10): badge 갱신만 — 강제 전환/모달 없음.
  // 같은 시점에 mission 화면의 mutation client seam도 연결한다(O16).
  useEffect(() => {
    setMissionClient(props.client);
    const stop = startMissionSync(props.client);
    return () => {
      stop();
      setMissionClient(null);
    };
  }, [props.client]);

  // Rust(트레이 "종료"·마지막 창 파괴)가 보낸 종료 요청 → 같은 확인 흐름.
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    void listenQuitRequested(() => controller.requestQuit())
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      })
      .catch(() => undefined);
    return () => {
      active = false;
      unlisten?.();
    };
  }, [controller]);

  useEffect(() => {
    controller.start();
    let active = true;
    void controller.restoreWorkspace().then(async () => {
      if (!active || useWorkbenchStore.getState().tabs.length !== 0) return;
      await detectShellProfiles(platform);
      if (active) startFirstTerminal();
    }).catch(() => { /* Controller reports discovery errors; never launch on failure. */ });
    return () => { active = false; controller.scheduleDispose(); };
    // startFirstTerminal은 StrictMode 2회 스케줄에 멱등이다(tabs 가드).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [controller, platform]);

  // 테마(system→해석 포함)·기본 글자 크기 변경을 살아 있는 모든 터미널에
  // 반영한다(W1-6). startThemeSync가 <html data-theme> CSS 쪽을 맡고,
  // 같은 해석 값을 xterm 테마로도 흘린다.
  // 기본 글자 크기 "자체"가 바뀐 경우에만 fontSize를 전달한다 — 그 외
  // 설정(테마·커서·스크롤백) 변경이 pane 줌을 리셋하지 않게(W2 검토).
  const lastBaseFontRef = useRef<number | null>(null);
  useEffect(() => {
    const applyToTerminals = (theme: ResolvedTheme): void => {
      const prefs = usePreferences.getState();
      const baseChanged =
        lastBaseFontRef.current !== null && lastBaseFontRef.current !== prefs.baseFontSize;
      lastBaseFontRef.current = prefs.baseFontSize;
      controller.registry.applyGlobalPreferences({
        theme: terminalTheme(theme),
        ...(baseChanged ? { fontSize: prefs.baseFontSize } : {}),
        // null(자동)은 지정이 아니라 판단 보류 — 터미널 생성 시점의
        // 리졸버 결과(내장/설치 폰트)를 그대로 쓰게 한다.
        ...(prefs.fontFamily !== null ? { fontFamily: prefs.fontFamily } : {}),
        cursorStyle: prefs.cursorStyle,
        scrollback: prefs.scrollbackLines,
      });
      // 테마가 바뀌면 tint 혼합 베이스도 바뀐다 — pane 배경색을 다시 계산한다.
      controller.applyAllPaneBackgrounds();
    };
    const stopTheme = startThemeSync(applyToTerminals);
    // 테마 변경은 startThemeSync가 흘려 주므로 여기서는 터미널 옵션에
    // 닿는 항목만 본다 — 글꼴 입력 한 글자마다 전체 pane 옵션을 다시 쓰지
    // 않게(W2 검토). OSC 52 토글도 같은 구독으로 반영한다.
    const stopPreferences = usePreferences.subscribe((state, prev) => {
      if (state.osc52Write !== prev.osc52Write) setOsc52Enabled(state.osc52Write);
      if (state.gpuRenderer !== prev.gpuRenderer) controller.registry.setRendererEnabled(state.gpuRenderer);
      if (
        state.baseFontSize !== prev.baseFontSize ||
        state.fontFamily !== prev.fontFamily ||
        state.cursorStyle !== prev.cursorStyle ||
        state.scrollbackLines !== prev.scrollbackLines
      ) {
        applyToTerminals(resolveTheme(state.theme, systemPrefersLight()));
      }
      // 에이전트별 배경 tint 설정 변경은 즉시 반영한다(테마와 무관하므로
      // 별도로 본다 — 글꼴 입력처럼 잦은 재적용을 피하지 않아도 된다).
      if (
        state.agentBackgrounds !== prev.agentBackgrounds ||
        state.agentBackgroundColors !== prev.agentBackgroundColors
      ) {
        controller.applyAllPaneBackgrounds();
      }
    });
    setOsc52Enabled(usePreferences.getState().osc52Write);
    return () => {
      stopTheme();
      stopPreferences();
      setOsc52Enabled(false);
    };
  }, [controller]);

  // 창 제목(W1-3): 포커스된 pane의 동적 제목(OSC 0/2)을 따라간다 — 작업
  // 표시줄/독에서 어느 터미널인지 식별하는 조용한 신호다. 에이전트가 작업
  // 중이면 "● ", 확인을 기다리면 "◌ "를 앞에 붙여 창이 뒤에 있어도 읽힌다.
  useEffect(() => {
    const apply = (): void => {
      const state = useWorkbenchStore.getState();
      const pane = state.focusedLeafId ? state.panes[state.focusedLeafId] : null;
      const title = pane ? terminalDisplayTitle(pane.title) : "";
      // 에이전트가 떠 있으면 현재 모델·effort도 붙인다(/model·/effort 즉시 반영).
      const model = pane?.agent ? agentModelLabel(pane.agent) : null;
      const parts = [title, model].filter((part): part is string => Boolean(part));
      const activity =
        pane?.agent && pane.phase === "live"
          ? resolveAgentActivity(
              pane.agent,
              typeof pane.sessionId === "string" && useTerminalActivity.getState().sessions.has(pane.sessionId),
            )
          : null;
      const prefix = activity === "working" ? "● " : activity === "waiting" ? "◌ " : "";
      const next = `${prefix}${parts.length > 0 ? `${parts.join(" — ")} — IYAGI Terminal` : "IYAGI Terminal"}`;
      if (document.title !== next) document.title = next;
    };
    apply();
    const unsubscribeWorkbench = useWorkbenchStore.subscribe(apply);
    const unsubscribeActivity = useTerminalActivity.subscribe(apply);
    return () => {
      unsubscribeWorkbench();
      unsubscribeActivity();
    };
  }, []);

  useEffect(() => {
    const onCompositionStart = () => {
      imeRef.current = true;
    };
    const onCompositionEnd = () => {
      imeRef.current = false;
    };
    window.addEventListener("compositionstart", onCompositionStart);
    window.addEventListener("compositionend", onCompositionEnd);
    return () => {
      window.removeEventListener("compositionstart", onCompositionStart);
      window.removeEventListener("compositionend", onCompositionEnd);
    };
  }, []);

  useEffect(() => {
    // 대화상자(미리보기·팔레트 등)는 자기 Esc 처리에서 모달을 곧바로 닫는다 — 아래 bubble 단계
    // 처리기에서는 이미 닫힌 뒤라, 누른 순간 모달이 떠 있었는지를 capture 단계에서 기억해 둔다.
    let modalOpenAtKeydown = false;
    const onKeyDownCapture = () => {
      modalOpenAtKeydown = useWorkbenchStore.getState().modal !== null;
    };
    const onKeyDown = (event: KeyboardEvent) => {
      const store = useWorkbenchStore.getState();
      if (store.page === "settings") return;
      // 배치 편집 화면: Esc는 터미널로 돌아가기. 대화상자를 닫는 Esc·조합 중인 Esc는 아니다(끄는 중의
      // Esc는 끌기 세션이, 이름 칸의 Esc는 그 칸이 먼저 가져간다).
      if (
        leavesLayoutPage(
          { key: event.key, isComposing: event.isComposing },
          { page: store.page, modalOpen: store.modal !== null || modalOpenAtKeydown },
        )
      ) {
        event.preventDefault();
        store.setPage("terminal");
        return;
      }
      // 탭 이름 인라인 편집 중에는 키보드 주인이 input이다.
      if (store.renamingTabId !== null) return;
      const ctx: ShortcutContext = {
        platform,
        imeComposing: imeRef.current || event.isComposing === true,
        modalOpen: store.modal !== null,
        hasSelection: controller.hasSelectionInFocusedPane(),
        overrides: usePreferences.getState().shortcutOverrides,
      };
      const action = mapShortcut(
        {
          key: event.key,
          code: event.code,
          ctrlKey: event.ctrlKey,
          metaKey: event.metaKey,
          altKey: event.altKey,
          shiftKey: event.shiftKey,
          repeat: event.repeat,
        },
        ctx,
      );
      if (action === null) return; // 터미널 입력 등 — 가로채지 않는다
      // 배치 편집 화면에서는 터미널이 숨어 있다 — 화면 전환·팔레트 말고는 터미널 단축키를 쓰지 않는다.
      if (store.page === "layout" && !layoutPageAllowsAction(action)) return;
      // 기본 붙여넣기 조합은 막지 않는다: 웹뷰가 스스로 붙여넣기를 실행해 아래
      // onPaste로 텍스트를 넘긴다. clipboard API로 읽으면 WebKit이 확인 말풍선을
      // 띄운다(nativePaste.ts).
      if (action === "paste" && isNativePasteKey(event, platform) && terminalLeafIdOf(event.target) !== null) {
        return;
      }
      event.preventDefault();
      controller.dispatchShortcut(action);
    };
    // 터미널에 닿은 네이티브 붙여넣기(단축키·편집 메뉴)는 xterm 기본 처리 대신 앱
    // 붙여넣기 경로(1 MiB 한도·정화·동시 입력)로 보낸다. capture 단계라 xterm의
    // textarea 리스너보다 먼저 받는다.
    const onPaste = (event: ClipboardEvent) => {
      const leafId = terminalLeafIdOf(event.target);
      if (leafId === null) return; // 입력 칸 등은 브라우저 기본 붙여넣기
      event.preventDefault();
      event.stopPropagation();
      const text = pastedText(event.clipboardData);
      // 글이 함께 왔으면 글이 이긴다 — 그림은 글이 없을 때의 대안이다.
      if (!text) {
        const image = imageFileOf(event.clipboardData);
        if (image) {
          void controller.pasteImage(image, leafId);
          return;
        }
      }
      controller.pasteText(text, leafId);
    };
    window.addEventListener("keydown", onKeyDownCapture, true);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("paste", onPaste, true);
    return () => {
      window.removeEventListener("keydown", onKeyDownCapture, true);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("paste", onPaste, true);
    };
  }, [controller, platform]);

  // 창에 파일을 떨어뜨리면 그 자리 pane에 경로를 붙여넣는다. Tauri 창은
  // dragDropEnabled가 켜져 있어 웹뷰에 HTML5 drop이 오지 않으므로 네이티브
  // 쪽 사건을 듣는다(features/app/fileDrop.ts). 좌표는 물리 픽셀이다.
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    void listenFileDrop((event) => {
      if (event.kind === "leave") {
        markDropTarget(null, document);
        return;
      }
      const point = toCssPoint(event.position, window.devicePixelRatio);
      const leafId = leafIdAtPoint(point.x, point.y, document);
      if (event.kind === "over") {
        markDropTarget(leafId, document);
        return;
      }
      markDropTarget(null, document);
      if (leafId !== null) controller.pasteDroppedPaths(event.paths, leafId);
    })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      })
      .catch(() => undefined);
    return () => {
      active = false;
      markDropTarget(null, document);
      unlisten?.();
    };
  }, [controller]);

  const tabs = useWorkbenchStore((s) => s.tabs);

  // 합치기·다시 묶기는 terminal 탭끼리의 일이다(05 §2) — mission 탭은 세지 않는다.

  const terminalTabCount = tabs.filter((tab) => tab.kind === "terminal").length;
  const activeTabId = useWorkbenchStore((s) => s.activeTabId);
  const activeTab = tabs.find((t) => t.id === activeTabId) ?? null;
  const setActiveTab = useWorkbenchStore((s) => s.setActiveTab);
  const openModal = useWorkbenchStore((s) => s.openModal);
  const renamingTabId = useWorkbenchStore((s) => s.renamingTabId);
  const startTabRename = useWorkbenchStore((s) => s.startTabRename);
  const renameTab = useWorkbenchStore((s) => s.renameTab);
  const toast = useWorkbenchStore((s) => s.toast);
  const setToast = useWorkbenchStore((s) => s.setToast);
  const page = useWorkbenchStore((s) => s.page);
  const settingsOpen = page === "settings";
  // 설정·배치 편집이 떠 있는 동안 워크벤치는 숨기만 한다(터미널·세션은 그대로 산다).
  const pageOpen = page !== "terminal";
  useEffect(() => {
    if (pageOpen) return;
    const frame = requestAnimationFrame(() => {
      const state = useWorkbenchStore.getState();
      // 모달·탭 이름 편집이 입력 초점을 가진 동안에는 터미널로 빼앗지 않는다.
      if (state.modal || state.renamingTabId !== null) return;
      const pane = state.focusedLeafId ? state.panes[state.focusedLeafId] : null;
      if (!pane) return;
      const entry = controller.registry.get(pane.viewId);
      entry?.refit();
      (entry?.terminal.element as HTMLElement | null)?.querySelector<HTMLTextAreaElement>("textarea")?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [pageOpen, controller]);

  // 트랙패드 두 손가락 가로 스와이프로 탭 전환(features/terminal/tabSwipe.ts).
  // capture 단계의 non-passive wheel 하나로 처리하고, 설정·배치 편집 화면이
  // 떠 있는 동안에는 붙이지 않는다(탭 막대가 보이지 않는다).
  useEffect(() => {
    const root = workbenchRef.current;
    if (pageOpen || !root) return;
    // 터미널 위에서도 동작한다: capture 리스너가 xterm보다 먼저 보고, 가로
    // 우위 제스처만 가로챈다(세로 스크롤은 인식기가 소비하지 않아 터미널에
    // 그대로 간다). mouse tracking 중인 TUI라도 가로 휠은 앱이 거의 안 쓴다.
    return attachTabSwipe(root, {
      prefs: () => usePreferences.getState().tabSwipe,
      cycle: (direction) =>
        useWorkbenchStore
          .getState()
          .cycleTab(direction === "next" ? 1 : -1, usePreferences.getState().tabSwipe.wrap),
    });
  }, [pageOpen]);

  // 활성 탭이 바뀌면 들어오는 탭에 슬라이드+페이드를 건다. 방향은 인덱스
  // 변화로, on/off·모션 최소화는 설정·CSS가 정한다. 리마운트 없이 같은
  // 요소에서 애니메이션을 재시작한다(none → 리플로 → 원복).
  useEffect(() => {
    const el = splitScrollRef.current;
    const nextIndex = useWorkbenchStore.getState().tabs.findIndex((tab) => tab.id === activeTabId);
    const dir = tabSwitchDirection(prevTabIndexRef.current, nextIndex);
    if (nextIndex >= 0) prevTabIndexRef.current = nextIndex;
    if (!el || pageOpen || dir === null) return;
    if (!usePreferences.getState().tabSwitchEffect) return;
    el.style.animation = "none";
    el.dataset.tabDir = dir;
    void el.offsetWidth; // 리플로로 애니메이션 재시작을 강제한다
    el.style.animation = "";
  }, [activeTabId, pageOpen]);

  // 탭 × · 탭 메뉴 · 메뉴 막대가 같은 닫기 규칙을 쓴다(mission 계열 탭은 로컬 숨김 — 05 §8).
  const closeTab = (tabId: string) => requestCloseTab(controller, tabId);

  return (
    <ControllerContext.Provider value={controller}>
      <div className="workbench" ref={workbenchRef} style={{ display: pageOpen ? "none" : undefined }}>
        <header className="top-bar">
          <nav className="tab-bar" aria-label={i18n.t("app.aria.tabs")}>
            {tabs.map((tab, index) => (
              <LiveTabItem
                key={tab.id}
                tab={tab}
                index={index}
                tabCount={tabs.length}
                terminalTabCount={terminalTabCount}
                active={tab.id === activeTabId}
                renaming={tab.id === renamingTabId}
                onSelect={() => setActiveTab(tab.id)}
                onClose={() => closeTab(tab.id)}
                onCloseAll={() => controller.requestCloseAllTabs()}
                onMoveLeft={() => controller.moveTab(tab.id, -1)}
                onMoveRight={() => controller.moveTab(tab.id, 1)}
                onMergeInto={() => openModal({ kind: "merge-tab", tabId: tab.id })}
                onRegroup={() => controller.regroupByProject()}
                onLayoutDragStart={(event) => startLayoutDrag(event, { kind: "tab", tabId: tab.id }, controller)}
                onRenameStart={() => startTabRename(tab.id)}
                onRenameCommit={(title) => {
                  renameTab(tab.id, title);
                  startTabRename(null);
                }}
                onRenameCancel={() => startTabRename(null)}
              />
            ))}
            <NewTabButton onNewTerminal={() => controller.newTab()} />
          </nav>
          <div className="top-actions">
            <NotificationBell />
            <BroadcastToggle />
            <LayoutEditorButton platform={platform} />
            <LanguageToggle />
            <NewMissionButton />
            <ShellPicker
              platform={platform}
              onNewTerminal={(shell) => controller.newTerminal(shell)}
            />
            <button
              type="button"
              className="icon-button"
              aria-label={i18n.t("settings.title")}
              title={i18n.t("settings.title")}
              onClick={() => openModal({ kind: "managed-run" })}
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <circle cx="12" cy="12" r="3" />
                <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h.01a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51h.01a1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v.01a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
              </svg>
            </button>
          </div>
        </header>
        <div className="workbench-main">
          <MissionCreateSidebar />
          <div className="split-scroll" ref={splitScrollRef}>
            {activeTab?.kind === "terminal" && activeTab.root ? (
              <SplitContainer tabId={activeTab.id} />
            ) : activeTab && activeTab.kind === "mission" ? (
              <MissionPage missionId={activeTab.missionId} />
            ) : activeTab && activeTab.kind === "agent-view" ? (
              <MissionAgentView missionId={activeTab.missionId} taskId={activeTab.taskId} />
            ) : (
              <EmptyProject platform={platform} />
            )}
          </div>
          <QueueDrawer />
        </div>
        <GraphDrawer />
        {/* 데몬이 오래됐을 때의 재시작 안내는 배너 대신 스트립의 작은 항목으로 — 배치를 흔들지 않는다. */}
        <ResourceStrip client={props.client} />
      </div>
      {settingsOpen ? <SettingsPage client={props.client} platform={platform} /> : null}
      {page === "layout" ? <LayoutEditorPage platform={platform} /> : null}
      {/* 토스트는 워크벤치 밖에 둔다 — 설정·배치 편집 화면이 떠 있어도(워크벤치 숨김) 막힌 이유·삭제 안내가 보여야 한다. */}
      {toast ? (
        <div className="toast" role="status" onClick={() => setToast(null)}>
          {toast}
        </div>
      ) : null}
      {/* 워크벤치가 숨겨진(설정 페이지) 동안에도 종료 확인 등 모달은 보여야 한다. */}
      <ModalHost platform={platform} />
      {/* 끌어 놓기 미리보기(04-ui §2-5) — 포털로 body 위에 그린다. */}
      <LayoutDragLayer />
    </ControllerContext.Provider>
  );
}

function EmptyProject(props: { platform: Platform }): JSX.Element {
  const controller = useController();
  const openModal = useWorkbenchStore((s) => s.openModal);
  const { t } = useI18n();
  const [candidates, setCandidates] = useState<CliCandidate[] | null>(null);

  // 퀵스타트 최소판(W1-10): 감지된 CLI 후보를 보여 주고 기본 관리 정책으로
  // 1-클릭 실행한다 — 정체성 증명이 첫 30분 안에 나도록 하는 활성화 단계.
  useEffect(() => {
    let active = true;
    const probe = getManagedRunDeps()?.probe ?? null;
    if (!probe) {
      setCandidates([]);
      return;
    }
    void discoverClis(probe).then((state) => {
      if (active) setCandidates(state.status === "ready" ? state.candidates : []);
    });
    return () => { active = false; };
  }, []);

  const quickstart = useMemo(() => {
    if (!candidates) return null;
    const kinds: CliCandidateKind[] = ["codex", "claude", "opencode"];
    const list = kinds
      .map((kind) => candidatesForKind(candidates, kind)[0])
      .filter((c): c is CliCandidate => Boolean(c))
      .filter((c) => !(props.platform === "windows" && isWindowsShellShim(c.program)));
    // 마지막으로 쓴 CLI를 맨 앞으로(W3-4) — 두 번째 방문부터 1-클릭이
    // 더 짧아진다.
    const last = getLastQuickstart();
    if (!last) return list;
    return [...list.filter((c) => c.program === last), ...list.filter((c) => c.program !== last)];
  }, [candidates, props.platform]);

  const runQuickstart = (candidate: CliCandidate) => {
    const name = candidate.kind === "claude" ? "claude" : candidate.kind === "opencode" ? "opencode" : "codex";
    // Claude 빠른 실행만 제공자 라우팅(Z.ai) 대상 — 구 데몬이면 실행하지 않고 알린다
    // (조용히 Anthropic으로 떨어지지 않는다).
    const routing = claudeProviderFor(
      candidate.kind,
      usePreferences.getState(),
      useWorkbenchStore.getState().claudeProviderRouting,
    );
    if (!routing.ok) {
      controller.toast(t(CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY));
      return;
    }
    rememberQuickstart(candidate.program);
    void controller.runManaged({
      program: candidate.program,
      argv: autonomyArgv(candidate.kind, [], autonomyEnabled(candidate.kind, usePreferences.getState())),
      cwd: controller.defaultCwd(),
      priority: 1,
      reservationBytes: "2147483648",
      title: name,
      claudeProvider: routing.provider,
    });
  };

  return (
    <div className="empty-project">
      <h2>{t("app.empty.title")}</h2>
      <p>{t("app.empty.body")}</p>
      <div className="empty-actions">
        <button type="button" className="primary" onClick={() => controller.newTerminal()}>
          {t("app.empty.newTerminal")}
        </button>
        <button type="button" onClick={() => openModal({ kind: "managed-run" })}>
          {t("app.empty.managedRun")}
        </button>
      </div>
      <EmptyMissionCard />
      {quickstart ? (
        <div className="empty-quickstart">
          <h3>{t("app.empty.cliSection")}</h3>
          <p className="muted">{t("app.empty.cliHint")}</p>
          {quickstart.length === 0 ? (
            <p className="muted">{t("app.empty.cliNone")}</p>
          ) : (
            <div className="empty-cli-list">
              {quickstart.map((candidate) => (
                <div className="empty-cli-entry" key={`${candidate.kind}:${candidate.program}`}>
                  <button
                    type="button"
                    className="empty-cli-card"
                    title={candidate.program}
                    onClick={() => runQuickstart(candidate)}
                  >
                    <span className="empty-cli-name">{candidate.kind}</span>
                    <span className="empty-cli-path">{candidate.program}</span>
                  </button>
                  <AutonomyToggle kind={candidate.kind} />
                </div>
              ))}
            </div>
          )}
        </div>
      ) : null}
    </div>
  );
}

/** 상단 KO/EN 토글 — 즉시 전체 UI가 전환 언어로 다시 렌더링된다. */
/**
 * 탭 막대의 `+`(05 §3): 누르는 즉시 새 터미널 탭을 연다. 새 AI 작업은
 * 명령 팔레트·네이티브 메뉴·pane 오른쪽 클릭 메뉴에서 연다.
 */
function NewTabButton(props: { onNewTerminal: () => void }): JSX.Element {
  const { t } = useI18n();
  return (
    <button
      type="button"
      className="tab-add"
      aria-label={t("app.newTab")}
      title={t("app.newTab")}
      data-testid="new-tab-button"
      onClick={props.onNewTerminal}
    >
      +
    </button>
  );
}

/**
 * 새 AI 작업 버튼(05 §3): 보고 있는 터미널의 저장소를 채워 대화상자를 연다.
 * mission 프로토콜이 없으면 숨기지 않고 비활성 + 사유(tooltip)로 설명한다.
 */
function NewMissionButton(): JSX.Element | null {
  const { t } = useI18n();
  const entry = useMissionEntryState();
  const overrides = usePreferences((s) => s.shortcutOverrides);
  // 프로덕션 빌드에서 데몬이 프로토콜을 선언하지 않으면 숨긴다(개발 빌드는 비활성 + 사유).
  if (entry.hidden) return null;
  const hint = shortcutHint("new-mission", currentPlatform(), overrides);
  // 쓸 수 있을 때도 무엇을 하는 버튼인지 한 줄로 알린다(발견성).
  const description = t("missions.newMission.hint");
  const title = entry.enabled
    ? hint
      ? `${description} (${hint})`
      : description
    : entry.reasonKey
      ? t(entry.reasonKey)
      : undefined;
  return (
    <>
      <button
        type="button"
        className="new-mission-button"
        disabled={!entry.enabled}
        title={title}
        data-testid="new-mission-button"
        onClick={() => openMissionCreate(focusedRepositoryHint(useWorkbenchStore.getState()))}
      >
        + {t("missions.newMission")}
      </button>
      {/* 닫은 작업 탭을 다시 여는 자리(작업은 탭과 무관하게 계속 실행된다). */}
      <button
        type="button"
        className="mission-list-button"
        disabled={!entry.enabled}
        aria-label={t("missions.list.title")}
        title={entry.enabled ? t("missions.list.title") : title}
        data-testid="mission-list-button"
        onClick={openMissionList}
      >
        ☰
      </button>
    </>
  );
}

/**
 * 터미널 빈 화면의 AI 작업 카드 한 장(발견성): 무엇을 하는지 한 줄 + 새 AI 작업·목록.
 * 프로덕션 빌드에서 데몬이 프로토콜을 선언하지 않으면 그리지 않는다.
 */
function EmptyMissionCard(): JSX.Element | null {
  const { t } = useI18n();
  const entry = useMissionEntryState();
  if (entry.hidden) return null;
  const reason = entry.reasonKey ? t(entry.reasonKey) : undefined;
  return (
    <section className="empty-mission-card" data-testid="empty-mission-card">
      <h3>{t("app.empty.missionTitle")}</h3>
      <p className="muted">{t("missions.newMission.hint")}</p>
      {!entry.enabled && reason ? <p className="muted">{reason}</p> : null}
      <div className="empty-mission-actions">
        <button
          type="button"
          disabled={!entry.enabled}
          onClick={() => openMissionCreate(focusedRepositoryHint(useWorkbenchStore.getState()))}
        >
          {t("app.empty.missionNew")}
        </button>
        <button type="button" disabled={!entry.enabled} onClick={openMissionList}>
          {t("app.empty.missionList")}
        </button>
      </div>
    </section>
  );
}

/**
 * 동기 입력 토글(04 §1): 기본 off인 명시적 기능이라 켜진 상태가 눈에 띄어야
 * 한다. 켤 때만 toast로 한 번 더 알린다 — 끄는 쪽은 위험하지 않다.
 * 라벨 텍스트 대신 방송(전파) 아이콘 — 이름은 aria-label·title이 유지한다.
 */
function BroadcastToggle(): JSX.Element {
  const { t: translate } = useI18n();
  const controller = useController();
  const on = useWorkbenchStore((s) => s.broadcastInput);
  const hint = translate(on ? "app.broadcast.on" : "app.broadcast.off");
  return (
    <button
      type="button"
      className={`broadcast-toggle icon-button${on ? " on" : ""}`}
      aria-pressed={on}
      aria-label={translate("app.broadcast.label")}
      title={hint}
      onClick={() => controller.toggleBroadcast()}
    >
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="1.8" />
        <path d="M16.24 7.76a6 6 0 0 1 0 8.49" />
        <path d="M7.76 16.24a6 6 0 0 1 0-8.49" />
        <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
        <path d="M4.93 19.07a10 10 0 0 1 0-14.14" />
      </svg>
    </button>
  );
}

/**
 * 상단 바 "배치 편집"(04-ui §2-6): 탭·터미널 재배치의 눈에 보이는 진입점. 메뉴·팔레트에만
 * 두면 기능이 있는 줄 모른다. 프로젝트별로 정리(예전 "탭 정리")는 그 화면의 머리에 있다.
 */
function LayoutEditorButton({ platform }: { platform: Platform }): JSX.Element {
  const { t: translate } = useI18n();
  const overrides = usePreferences((s) => s.shortcutOverrides);
  const hint = shortcutHint("layout-editor", platform, overrides);
  return (
    <button
      type="button"
      className="layout-editor-button icon-button"
      aria-label={translate("layoutEditor.open")}
      title={hint ? `${translate("layoutEditor.openHint")} (${hint})` : translate("layoutEditor.openHint")}
      onClick={() => useWorkbenchStore.getState().setPage("layout")}
    >
      {/* 창이 나뉜 모양 — 탭·터미널 재배치. 이름은 title이 유지한다. */}
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <path d="M3 9h18" />
        <path d="M9 9v12" />
      </svg>
    </button>
  );
}

function LanguageToggle(): JSX.Element {
  const { language, t } = useI18n();
  const setLanguage = useI18nStore((s) => s.setLanguage);
  const next: Language = language === "ko" ? "en" : "ko";
  return (
    <button
      type="button"
      className="lang-toggle icon-button"
      aria-label={t("app.lang.toggle")}
      // 지구본 아이콘에는 현재 언어가 안 보이므로 title에 전환 방향을 함께 적는다.
      title={`${t("app.lang.toggle")} (${language.toUpperCase()} → ${next.toUpperCase()})`}
      onClick={() => setLanguage(next)}
    >
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9" />
        <path d="M3 12h18" />
        <path d="M12 3a13.5 13.5 0 0 1 3.5 9A13.5 13.5 0 0 1 12 21a13.5 13.5 0 0 1-3.5-9A13.5 13.5 0 0 1 12 3z" />
      </svg>
    </button>
  );
}
