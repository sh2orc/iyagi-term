/**
 * TerminalRegistry (04-ui.md §4): session/view → xterm instance,
 * subscriptions, input FIFO, ResizeObserver, attach state — kept OUTSIDE
 * React on purpose.
 *
 * Lifecycle contract (U09):
 * - `acquire(viewId)` is idempotent: React StrictMode's mount→cleanup→mount
 *   must produce exactly ONE terminal creation.
 * - `mount(parent)` / `unmount()` manage DOM + subscriptions ONLY. Session
 *   create/kill NEVER happens here — the registry has no client reference,
 *   so effect cleanup structurally cannot launch or detach sessions.
 * - Hidden panes (다른 탭) keep their xterm alive; only an explicit
 *   `disposeView` (명시적 detach/닫기) disposes the instance — the next
 *   attach replays from the journal.
 */

import { lineHeightForFont } from "./defaultFont";
import { readablePaletteFor } from "./terminalPalette";
import { preserveViewport, type ViewportTerminal } from "./viewport";
import { beginZoomPreview, cancelZoomPreview, observeHistoryRedraws, zoomPreviewFor } from "./zoomPreview";

/**
 * 동시에 살려 둘 WebGL 렌더러 상한. 탭을 바꿔도 최근 탭들의 렌더러를 살려
 * 두어 재부착이 즉시·매끄럽게 되지만, 브라우저의 WebGL 컨텍스트 예산(대략
 * 16개)을 넘기면 우리 것이 임의로 lost 되므로 여유를 두고 묶는다. 보이는
 * pane(활성 탭, 최대 8개)은 항상 유지하고, 초과분은 가장 오래된 숨은 pane의
 * 렌더러부터 회수한다. xterm 인스턴스·버퍼는 그대로 살아 있어 재생성 비용만
 * 한 번 더 드는 정도다.
 */
const RENDERER_CONTEXT_CAP = 12;
/**
 * 컨텍스트를 잃은 WebGL 렌더러를 다시 만들기까지의 대기(보이는 pane만). DOM
 * 렌더러로 오래 남으면 대체 글꼴 글리프(스피너·기호)마다 줄 기준선이 달라져
 * 새 출력마다 글자가 위아래로 흔들린다. 즉시 재생성은 실패 루프가 되므로
 * 간격을 두고 세 번만 시도한다.
 */
const RENDERER_RETRY_DELAYS_MS = [2000, 8000, 30000];

/** Minimal terminal surface the registry/pipeline depend on. */
export interface TerminalLike extends ViewportTerminal {
  open(element: unknown): void;
  write(data: string | Uint8Array, callback?: () => void): void;
  resize(cols: number, rows: number): void;
  /**
   * 현재 격자의 행 수(`cols`는 ViewportTerminal에 이미 있다). xterm Terminal은
   * 항상 가진다 — node 테스트 가짜는 생략할 수 있고, 그러면 fit의 첫 덮음
   * 판정 기준에서 제외된다(undefined ≠ 어떤 유한한 dims).
   */
  rows?: number;
  /** Redraw the viewport after its DOM is reattached, even if its grid is unchanged. */
  refresh?(start: number, end: number): void;
  dispose(): void;
  /** Clear parser/screen state before a full journal replay. */
  reset?(): void;
  onData(callback: (data: string) => void): { dispose(): void };
  hasSelection(): boolean;
  getSelection(): string;
  /** 전체 선택(컨텍스트 메뉴). node 테스트 가짜는 생략할 수 있다. */
  selectAll?(): void;
  /** 이 창의 화면·scrollback만 지운다 — 셸에는 아무것도 보내지 않는다. */
  clear?(): void;
  focus?(): void;
  /**
   * DEC 모드 상태. mouseTrackingMode가 "none"이 아니면 앱(vim·htop 등)이
   * 마우스를 받고 있어 오른쪽 클릭도 그 앱의 입력이다.
   */
  modes?: { mouseTrackingMode?: string; bracketedPasteMode?: boolean };
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void;
  loadAddon(addon: unknown): void;
  element: unknown;
  /**
   * Live xterm option surface (xterm 5+ allows assignment, e.g.
   * `term.options.fontSize = 14`). Optional so node-test fakes without
   * zoom support keep compiling; setFontSize no-ops when absent.
   */
  options?: {
    fontSize: number;
    theme?: unknown;
    fontFamily?: string;
    lineHeight?: number;
    cursorStyle?: "block" | "underline" | "bar";
    scrollback?: number;
    linkHandler?: unknown;
  };
  /**
   * 렌더 직후(격자가 그려진 뒤)마다 불리는 구독. WebGL → DOM 폴백이나 DPR
   * 변경으로 셀 메트릭이 바뀌면 registry가 이 신호로 다시 fit한다. node 테스트
   * 가짜는 생략할 수 있고, 그러면 셀 변화 감시가 꺼진다.
   */
  onRender?(callback: () => void): { dispose(): void };
}

/**
 * 셀 메트릭 읽기에 필요한 xterm 내부 구조. 실제 Terminal은
 * `_core._renderService.dimensions.css.cell`에 현재 셀 폭·높이(css px)를
 * 들고 있다 — 렌더러(WebGL/DOM)가 갈아끼워질 때 이 값이 바뀐다.
 */
interface CellMetricsSource {
  _core?: {
    _renderService?: {
      dimensions?: { css?: { cell?: { width: unknown; height: unknown } } };
    };
  };
}

/** 현재 셀 메트릭(읽을 수 없으면 null — 가짜 터미널·구형 xterm). */
function cellMetricsOf(terminal: TerminalLike): { width: number; height: number } | null {
  const cell = (terminal as CellMetricsSource)._core?._renderService?.dimensions?.css?.cell;
  if (!cell) return null;
  const width = Number(cell.width);
  const height = Number(cell.height);
  if (!Number.isFinite(width) || !Number.isFinite(height)) return null;
  return { width, height };
}

export interface FitAddonLike {
  proposeDimensions(): { cols: number; rows: number } | undefined;
}

export interface ResizeObserverLike {
  observe(target: unknown): void;
  unobserve(target: unknown): void;
  disconnect(): void;
}

export type TerminalFactory = () => TerminalLike;
export type FitAddonFactory = () => FitAddonLike;

/** mount 동안만 살아 있는 렌더러 부착물(GPU 렌더러 등). */
export interface RendererLike {
  dispose(): void;
  /** Invalidate cached GPU drawing state when a retained renderer is reattached. */
  refresh?(): void;
}
/**
 * 렌더러 팩토리가 받는 사고 갈고리. 렌더러가 저절로 죽으면(WebGL 컨텍스트
 * 상실 등) `onDead`를 불러 준다 — 받은 쪽은 그 래퍼를 더 살아 있다고 세지
 * 말아야 한다.
 */
export interface RendererHooks {
  onDead?: () => void;
}
/**
 * mount마다 렌더러를 만든다(null이면 xterm 기본 DOM 렌더러 그대로). 보이는
 * pane만 GPU 컨텍스트를 쥐도록 unmount 때 해제하고 다음 mount 때 다시
 * 만든다 — 숨은 탭의 터미널은 살아 있지만 컨텍스트는 잡지 않는다.
 */
export type RendererFactory = (terminal: TerminalLike, hooks: RendererHooks) => RendererLike | null;
export type ResizeObserverFactory = (callback: () => void) => ResizeObserverLike;
export type DomFactory = () => RegistryDom;

/** DOM node the registry owns per terminal (a wrapper div in the app). */
export interface RegistryDom {
  className: string;
  parentElement: { getBoundingClientRect(): { width: number; height: number } } | null;
  remove(): void;
}

export interface RegistryDeps {
  createTerminal: TerminalFactory;
  createFitAddon?: FitAddonFactory;
  /** defaults to the global ResizeObserver when available. */
  createResizeObserver?: ResizeObserverFactory;
  createDom?: DomFactory;
  /** 보이는 pane에 붙일 렌더러(WebGL). 없으면 DOM 렌더러. */
  createRenderer?: RendererFactory;
  /** 렌더러 사용 여부의 초기값(설정 `gpuRenderer`; 기본 켬). */
  rendererEnabled?: boolean;
  /**
   * 터미널이 새로 open된 직후 1회(IME 조정기 부착 등 브라우저 전용 후크).
   * 해제 함수를 돌려주면 `dispose()`가 xterm을 버리기 직전에 부른다 —
   * host에 붙인 리스너·타이머·전역 참조가 인스턴스와 함께 사라지게.
   */
  onOpen?: (terminal: TerminalLike, dom: unknown) => void | (() => void);
}

export interface FitDimensions {
  cols: number;
  rows: number;
  /** host element size in px (split-space checks use these) */
  width: number;
  height: number;
}

/** Anything that can hold the terminal container (an element in the app). */
export interface MountPoint {
  appendChild(child: unknown): unknown;
}

export interface RegistryEntry {
  readonly viewId: string;
  readonly terminal: TerminalLike;
  /** Attach DOM+subscriptions; safe to call repeatedly. */
  mount(parent: MountPoint): void;
  /** DOM+subscriptions only — the terminal instance survives (U09). */
  unmount(): void;
  /** Full disposal (explicit detach). Next attach replays via journal. */
  dispose(): void;
  /** Recompute fit dimensions and notify the fit handler. */
  refit(): void;
  /**
   * Apply a new font size (pane zoom). Returns true when the size actually
   * changed and a refit was triggered — the refit → fit → session.resize
   * path recomputes cols/rows for the new glyph metrics.
   */
  setFontSize(size: number): boolean;
  /** Fit output consumer (the session pipeline). */
  setFitHandler(handler: ((dims: FitDimensions) => void) | null): void;
  /**
   * 지금 host에 맞는 격자(mount되어 실측 가능할 때만). 새 pane의 PTY를 처음부터
   * 창 크기로 열 때 쓴다 — 붙은 뒤 fit이 80×24를 다시 맞추는 왕복을 없앤다.
   */
  measure(): { cols: number; rows: number } | null;
}

class RegistryEntryImpl implements RegistryEntry {
  mounted = false;
  private resizeObserver: ResizeObserverLike | null = null;
  private fitAddon: FitAddonLike | null = null;
  private fitHandler: ((dims: FitDimensions) => void) | null = null;
  private lastFit: { cols: number; rows: number } | null = null;
  /** WebGL 렌더러. 숨겨도(unmount) 살려 두고 재생성 없이 재부착한다.
   *  회수는 registry의 LRU 예산·설정 끔·dispose에서만. */
  private renderer: RendererLike | null = null;
  /** 렌더러 LRU 정렬값(registry가 show/hide 때 증가시킨다). */
  lruStamp = 0;
  /** 컨텍스트 상실 뒤 재생성 시도 타이머·횟수. */
  private rendererRetryTimer: ReturnType<typeof setTimeout> | null = null;
  private rendererRetries = 0;
  /** registry가 스스로 렌더러를 해제하는 중 — 그 onDead는 상실이 아니다. */
  private droppingRenderer = false;
  /** 글꼴 변경 뒤 한 프레임 늦은 재측정(setFontSize) — 연타 때 하나만 남긴다. */
  private fontFitFrame: ReturnType<typeof requestAnimationFrame> | null = null;
  /** 셀 메트릭 변화 감시 해제(mount 동안만 살아 있다). */
  private stopRenderWatch: (() => void) | null = null;
  /** 렌더 뒤 셀 폭 비교 기준. null이면 메트릭을 읽을 수 없는 가짜/구형 터미널. */
  private cellBasis: { width: number; height: number } | null = null;
  /** 셀 변화로 예약된 refit 프레임 — 연속 렌더를 한 프레임에 하나로 접는다. */
  private cellFitFrame: ReturnType<typeof requestAnimationFrame> | null = null;
  private stopHistoryObserver: (() => void) | null;

  constructor(
    readonly viewId: string,
    readonly terminal: TerminalLike,
    readonly dom: RegistryDom,
    private readonly registry: TerminalRegistry,
    private readonly deps: RegistryDeps,
    /** `onOpen`이 돌려준 해제 함수(없으면 null). */
    private openCleanup: (() => void) | null,
  ) {
    this.stopHistoryObserver = observeHistoryRedraws(terminal);
  }

  mount(parent: MountPoint): void {
    if (this.mounted && Object.is(this.dom.parentElement, parent)) {
      this.refit();
      return;
    }
    // React Fast Refresh can replace the host node without running the old
    // effect cleanup. A boolean alone is therefore not proof that the xterm
    // DOM is still attached to the requested pane.
    if (this.mounted) {
      this.resizeObserver?.disconnect();
      this.resizeObserver = null;
      this.dom.remove();
      this.mounted = false;
    }
    parent.appendChild(this.dom);
    this.mounted = true;
    this.lastFit = null;
    if (!this.fitAddon && this.deps.createFitAddon) {
      this.fitAddon = this.deps.createFitAddon();
      try {
        this.terminal.loadAddon(this.fitAddon);
      } catch {
        this.fitAddon = null;
      }
    }
    if (!this.resizeObserver && this.deps.createResizeObserver) {
      this.resizeObserver = this.deps.createResizeObserver(() => this.refit());
      this.resizeObserver.observe(parent);
    }
    const retainedRenderer = this.renderer;
    this.syncRenderer();
    // A retained canvas can have stale texture/model state after being detached.
    // Equal dimensions do not trigger resize, so fit alone cannot repair it.
    if (retainedRenderer && this.renderer === retainedRenderer) this.renderer.refresh?.();
    this.registry.onEntryShown(this);
    this.refit();
    this.watchCellMetrics();
    if (this.terminal.rows) this.terminal.refresh?.(0, this.terminal.rows - 1);
  }

  unmount(): void {
    cancelZoomPreview(this.terminal);
    this.cancelFontFitFrame();
    this.cancelCellRefit();
    this.stopRenderWatch?.();
    this.stopRenderWatch = null;
    // 숨은 pane은 되살릴 필요가 없다 — 다음 mount가 syncRenderer로 다시 만든다.
    this.clearRendererRetry();
    // DOM + subscriptions only. 세션 생성/kill은 여기 없다(U09).
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
    this.dom.remove();
    this.mounted = false;
    this.lastFit = null;
    // 렌더러는 폐기하지 않는다 — 재부착 시 WebGL 재생성을 피한다.
    // 복귀 때 캐시 무효화와 전체 repaint로 분리 중 남은 화면 상태를 복구한다.
    // registry가 LRU 상한을 넘으면 가장 오래된
    // 숨은 pane의 렌더러만 회수한다(WebGL 컨텍스트 예산 보호).
    this.registry.onEntryHidden(this);
  }

  /**
   * 렌더러를 현재 상태(mount 여부 × 설정)에 맞춘다: 보이는 pane에 켜져
   * 있으면 하나 만들고, 아니면 해제한다. 생성 실패는 DOM 렌더러로 조용히
   * 물러난다 — WebGL이 없다고 터미널이 멈추면 안 된다(04-ui §4).
   */
  syncRenderer(): void {
    const wanted = this.mounted && this.registry.rendererEnabled && !!this.deps.createRenderer;
    if (!wanted) {
      this.dropRenderer();
      return;
    }
    if (this.renderer) return;
    try {
      this.renderer = this.deps.createRenderer?.(this.terminal, {
        onDead: () => {
          // 컨텍스트를 잃어 내부에서 폐기됐다 — 래퍼를 비워 다음 mount/syncRenderer가
          // WebGL을 다시 만들 수 있게 한다. 즉시 재생성하지 않고(실패 루프 방지)
          // 잠시 뒤 다시 시도한다 — vendored 애드온이 DOM 렌더러로 되돌려 놓아
          // 화면은 유지되지만, 거기 오래 남으면 글자가 흔들린다.
          this.renderer = null;
          if (!this.droppingRenderer) this.scheduleRendererRetry();
        },
      }) ?? null;
      if (this.renderer) this.rendererRetries = 0;
    } catch {
      this.renderer = null;
    }
  }

  /** 컨텍스트 상실 뒤 재생성 예약(보이는 동안만, RENDERER_RETRY_DELAYS_MS 순서로). */
  private scheduleRendererRetry(): void {
    if (this.rendererRetryTimer !== null || this.rendererRetries >= RENDERER_RETRY_DELAYS_MS.length) return;
    const delay = RENDERER_RETRY_DELAYS_MS[this.rendererRetries];
    this.rendererRetries += 1;
    this.rendererRetryTimer = setTimeout(() => {
      this.rendererRetryTimer = null;
      if (!this.mounted || this.renderer || !this.registry.rendererEnabled) return;
      this.syncRenderer();
      // 되살렸으면 LRU·예산에 다시 넣는다(만들지 못했으면 다음 시도가 이어진다).
      if (this.renderer) this.registry.onEntryShown(this);
      else this.scheduleRendererRetry();
    }, delay);
  }

  private clearRendererRetry(): void {
    if (this.rendererRetryTimer !== null) clearTimeout(this.rendererRetryTimer);
    this.rendererRetryTimer = null;
  }

  private dropRenderer(): void {
    this.clearRendererRetry();
    const renderer = this.renderer;
    this.renderer = null;
    this.droppingRenderer = true;
    try {
      renderer?.dispose();
    } catch {
      // 이미 잃은 컨텍스트의 정리 실패가 unmount를 막아선 안 된다.
    } finally {
      this.droppingRenderer = false;
    }
  }
  /** registry LRU 예산 판정용 — 살아 있는 WebGL 렌더러가 있는가. */
  hasLiveRenderer(): boolean {
    return this.renderer !== null;
  }
  /** 예산 초과 시 registry가 숨은 pane의 렌더러만 회수한다(xterm 인스턴스는 유지). */
  releaseRendererForBudget(): void {
    this.dropRenderer();
  }

  dispose(): void {
    this.unmount();
    this.dropRenderer(); // unmount는 이제 렌더러를 살려 두므로 여기서 확실히 회수
    this.fitHandler = null;
    const cleanup = this.openCleanup;
    this.openCleanup = null;
    this.stopHistoryObserver?.();
    this.stopHistoryObserver = null;
    try {
      cleanup?.();
    } catch {
      // 후크 해제 실패가 xterm 폐기를 막아선 안 된다.
    }
    this.terminal.dispose();
    this.registry.remove(this.viewId);
  }

  refit(): void {
    this.fit();
  }

  private fit(): void {
    // 숨은 탭의 터미널은 DOM에서 떼어져 있다(Workbench는 활성 탭만 그린다).
    // 떼어진 host에서 FitAddon은 NaN 크기를 내놓으므로 아예 계산하지 않는다.
    if (!this.mounted) return;
    const rect = this.dom.parentElement?.getBoundingClientRect();
    // Settings keeps the terminal DOM mounted but hidden. Do not shrink
    // live PTYs to FitAddon's minimum dimensions while it has no layout.
    if (rect && (rect.width <= 0 || rect.height <= 0)) return;
    const dims = this.fitAddon?.proposeDimensions();
    if (!dims) return;
    if (!this.fitHandler) {
      // 아직 세션이 붙지 않은 터미널(새 pane의 첫 mount): 저널 순서를 따를
      // pipeline이 없으므로 격자를 곧바로 host에 맞춘다 — 기본 80×24로 작게
      // 그려졌다가 첫 크기 기록에서 펴지는 순간을 없앤다. 붙은 뒤의 크기
      // 변경은 pipeline이 저널 순서로 적용한다.
      if (dims.cols !== this.terminal.cols || dims.rows !== this.terminal.rows) {
        this.terminal.resize(dims.cols, dims.rows);
      }
      return;
    }
    // Window resizing and font zoom use the same presentation transaction.
    // A rebuilding CLI's reflowed old buffer is only an intermediate layout;
    // keep it covered until the CLI has drawn the requested grid.
    // 숨은 동안 크기가 바뀌어 돌아온 탭도 처음 fit을 덮는다 — 직전 격자가
    // 없으면(mount는 lastFit을 비운다) 터미널의 현재 격자를 기준으로 삼는다.
    // 새 터미널의 첫 mount(기본 80x24 ≠ 실측 격자)도 덧침 대상이 되지만,
    // 빈 화면의 스냅샷을 덮었다가 첫 resize 기록 적용 때 그대로 드러난다.
    const prev = this.lastFit ?? { cols: this.terminal.cols, rows: this.terminal.rows };
    if ((dims.cols !== prev.cols || dims.rows !== prev.rows) && !this.screenIsEmpty()) {
      beginZoomPreview(this.terminal);
    }
    this.lastFit = { cols: dims.cols, rows: dims.rows };
    zoomPreviewFor(this.terminal)?.expectResize(dims.cols, dims.rows);
    this.fitHandler({
      cols: dims.cols,
      rows: dims.rows,
      width: rect?.width ?? dims.cols * 8,
      height: rect?.height ?? dims.rows * 16,
    });
  }

  /**
   * 화면(뷰포트)에 보일 내용이 한 글자라도 있는가 — 덮을 게 있는지의 판정.
   * 아직 출력이 없는 새 pane의 첫 fit은 빈 화면이라 덧침이 첫 페인트만
   * 늦출 뿐이다(빈 화면의 reflow는 보이지 않는다). 프롬프트 한 줄이라도
   * 찍혀 있으면 일반 경로로 덮는다. 판정은 xterm 공개 buffer API로만 하고,
   * 못 하면 안전하게 덮는 쪽으로 돌아간다.
   */
  private screenIsEmpty(): boolean {
    const buffer = this.terminal.buffer?.active;
    if (!buffer?.getLine) return false;
    const rows = this.terminal.rows ?? 0;
    for (let index = 0; index < rows; index += 1) {
      const line = buffer.getLine(buffer.viewportY + index) as
        | { translateToString?(trimRight?: boolean): string }
        | undefined;
      if ((line?.translateToString?.(true) ?? "").length > 0) return false;
    }
    return true;
  }

  setFontSize(size: number): boolean {
    const options = this.terminal.options;
    if (!options || options.fontSize === size) return false;
    // 글꼴만 바뀌고 격자(cols/rows)는 데몬 왕복 뒤에 바뀐다 — 그 사이를 덮지
    // 않으면 줄바꿈이 한 박자 늦게 다시 맞춰져 두 번에 나눠 바뀌어 보인다.
    if (this.mounted) beginZoomPreview(this.terminal);
    preserveViewport(this.terminal, () => { options.fontSize = size; });
    // 글자 메트릭이 바뀌면 같은 픽셀 폭에 들어가는 cols/rows가 달라진다.
    // 즉시 refit이 동기 경로를 잡고, rAF 한 번 더 돌아 폰트 측정이 늦게
    // 갱신되는 경우(비동기 로드)까지 수용한다. 연타 중에는 그 늦은 재측정을
    // 하나로 접는다 — 프레임마다 proposeDimensions(강제 레이아웃)를 쌓지 않는다.
    this.fit();
    if (typeof requestAnimationFrame === "function") {
      this.cancelFontFitFrame();
      this.fontFitFrame = requestAnimationFrame(() => {
        this.fontFitFrame = null;
        this.fit();
      });
    }
    return true;
  }

  private cancelFontFitFrame(): void {
    if (this.fontFitFrame === null) return;
    if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(this.fontFitFrame);
    this.fontFitFrame = null;
  }

  /**
   * 렌더 뒤 셀 메트릭 감시를 (다시) 건다. WebGL 렌더러는 셀 폭을 기기
   * 픽셀로 내리고(Math.floor) DOM 렌더러는 내리지 않는다 — 컨텍스트 상실로
   * 폴백하거나 DPR이 바뀌면 컨테이너는 그대로인데 셀만 넓어진다.
   * ResizeObserver는 부르지 않으므로(크기가 안 바뀌었다) 여기서 직접 비교해
   * 다시 fit한다. 안 그러면 옛 cols × 넓어진 셀이 pane을 넘어 그려진다.
   */
  private watchCellMetrics(): void {
    this.stopRenderWatch?.();
    this.stopRenderWatch = null;
    const basis = cellMetricsOf(this.terminal);
    this.cellBasis = basis;
    if (basis === null || typeof this.terminal.onRender !== "function") return;
    let subscription: { dispose(): void };
    try {
      subscription = this.terminal.onRender(() => this.onRenderCheckCells());
    } catch {
      return; // 부착 실패는 폴백 상태의 화면 치우침보다 치명적이지 않다.
    }
    this.stopRenderWatch = () => subscription.dispose();
  }

  /** 렌더 콜백: 격자를 그리는 도중이므로 여기서는 비교만 하고 fit은 다음 프레임에. */
  private onRenderCheckCells(): void {
    const metrics = cellMetricsOf(this.terminal);
    if (metrics === null) return;
    const changed =
      this.cellBasis === null ||
      metrics.width !== this.cellBasis.width ||
      metrics.height !== this.cellBasis.height;
    // 기준은 지금 값으로 옮긴다 — 바뀌지 않은 이후 렌더가 다시 예약하지 않게.
    this.cellBasis = metrics;
    if (changed) this.scheduleCellRefit();
  }

  private scheduleCellRefit(): void {
    if (this.cellFitFrame !== null) return;
    if (typeof requestAnimationFrame !== "function") {
      this.fit();
      return;
    }
    this.cellFitFrame = requestAnimationFrame(() => {
      this.cellFitFrame = null;
      // 0크기 레이아웃 등 fit이 스스로 물러나는 경우에도 기준은 이미 옮겼으므
      // 로 레이아웃이 돌아오면 ResizeObserver가, 셀이 또 바뀌면 렌더가 다시
      // 잡는다 — 헛된 fit을 되풀이하지 않는다.
      this.fit();
    });
  }

  private cancelCellRefit(): void {
    if (this.cellFitFrame === null) return;
    if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(this.cellFitFrame);
    this.cellFitFrame = null;
  }

  setFitHandler(handler: ((dims: FitDimensions) => void) | null): void {
    this.fitHandler = handler;
  }

  measure(): { cols: number; rows: number } | null {
    if (!this.mounted) return null;
    const rect = this.dom.parentElement?.getBoundingClientRect();
    if (rect && (rect.width <= 0 || rect.height <= 0)) return null;
    const dims = this.fitAddon?.proposeDimensions();
    if (!dims) return null;
    const fits = (n: number): boolean => Number.isInteger(n) && n >= 2 && n <= 1000;
    return fits(dims.cols) && fits(dims.rows) ? { cols: dims.cols, rows: dims.rows } : null;
  }
}

/**
 * 테마 객체에 pane 배경색을 얹는다(pane별 배경 — paneBackground.ts). 커서
 * 반대색도 함께 맞춘다(내장 테마가 background==cursorAccent를 쓰는 규칙).
 * color가 null이면 테마를 그대로 돌려준다.
 *
 * 배경 명암이 테마와 뒤집히면 글자·ANSI 색도 같이 바꾼다 — 밝은 테마에서
 * 에이전트 tint는 어두운 면이라(paneBackground.tintedSurface) 밝은 테마의
 * 검은 글자를 그대로 두면 글자가 배경에 묻힌다. 반대(어두운 테마 + 사용자가
 * 고른 밝은 배경)도 같은 이유로 밝은 테마 팔레트로 바꾼다. 배경만 바꿔도
 * 되는 경우(명암이 테마와 같은 방향)는 지금 팔레트를 그대로 둔다.
 */
const withBackground = (theme: unknown, color: string | null): unknown => {
  if (color === null) return theme;
  const base = (theme ?? {}) as Record<string, unknown>;
  const foreground = typeof base.foreground === "string" ? base.foreground : null;
  const palette = foreground === null ? null : readablePaletteFor(color, foreground);
  return { ...base, ...palette, background: color, cursorAccent: color };
};

export class TerminalRegistry {
  private readonly entries = new Map<string, RegistryEntryImpl>();
  /** 설정 `gpuRenderer` — mount된 pane의 렌더러 생성/해제 기준. */
  rendererEnabled: boolean;
  /**
   * 마지막으로 전역 적용한 기본 테마 — pane별 배경 덮어쓰기의 베이스.
   * null은 아직 전역 적용이 없었다는 뜻(터미널 생성 시점 값을 쓴다).
   */
  private baseTheme: unknown = null;
  /** view별 배경 덮어쓰기(에이전트 tint·pane 사용자 지정). */
  private readonly backgroundOverrides = new Map<string, string>();
  /**
   * pane 표시/숨김 관찰자(controller가 설정). show/hide는 이미 mount/unmount가
   * 아는 사실이므로 여기서 한 번 더 알린다 — controller가 숨은 view의 pipeline
   * 표시 빈도(paint throttle)를 낮추는 데 쓴다(04-ui §4 "표시 빈도만 줄인다").
   * WebGL 렌더러 유지(LRU)와는 독립이다.
   */
  private visibilityListener: ((viewId: string, visible: boolean) => void) | null = null;

  constructor(private readonly deps: RegistryDeps) {
    this.rendererEnabled = deps.rendererEnabled ?? true;
  }

  /** 설정 토글: 보이는 pane 전부에 즉시 반영한다(켜면 생성, 끄면 해제). */
  setRendererEnabled(enabled: boolean): void {
    if (this.rendererEnabled === enabled) return;
    this.rendererEnabled = enabled;
    for (const entry of this.entries.values()) entry.syncRenderer();
  }

  /**
   * 표시/숨김 관찰자를 건다(controller 전용, 한 슬롯). `null`로 해제한다.
   * mount → onEntryShown(visible=true), unmount → onEntryHidden(visible=false)로
   * 부른다. 아직 pipeline이 없는 view도 통보되며, 그쪽에서 no-op 처리한다.
   */
  setVisibilityListener(listener: ((viewId: string, visible: boolean) => void) | null): void {
    this.visibilityListener = listener;
  }
  /** 렌더러 LRU 순번 발급기(단조 증가). */
  private lruCounter = 0;
  /** pane이 보이게 됨(mount): LRU 갱신 후 예산을 지킨다. */
  onEntryShown(entry: RegistryEntryImpl): void {
    entry.lruStamp = ++this.lruCounter;
    this.enforceRendererBudget();
    // LRU 판정과 무관한 표시 통보 — 숨김 동안 낮췄던 표시 빈도를 되돌리게 한다.
    this.visibilityListener?.(entry.viewId, true);
  }
  /** pane이 숨겨짐(unmount): 렌더러는 살아 있으므로 예산을 지킨다. */
  onEntryHidden(entry: RegistryEntryImpl): void {
    entry.lruStamp = ++this.lruCounter;
    this.enforceRendererBudget();
    // 렌더러는 살아 있어 계속 그릴 수 있으므로 표시 빈도를 낮추라고 통보한다.
    this.visibilityListener?.(entry.viewId, false);
  }
  /**
   * 살아 있는 WebGL 렌더러 수를 상한 이하로 유지한다. 회수 대상은 숨은
   * (unmount된) pane뿐이며 가장 오래 안 쓴 것부터 — 보이는 pane의 렌더러는
   * 절대 건드리지 않는다.
   */
  private enforceRendererBudget(): void {
    const live = [...this.entries.values()].filter((entry) => entry.hasLiveRenderer());
    let over = live.length - RENDERER_CONTEXT_CAP;
    if (over <= 0) return;
    const evictable = live
      .filter((entry) => !entry.mounted)
      .sort((a, b) => a.lruStamp - b.lruStamp);
    for (const entry of evictable) {
      if (over <= 0) break;
      entry.releaseRendererForBudget();
      over -= 1;
    }
  }
  /** 감사/테스트용: 지금 살아 있는 WebGL 렌더러 수. */
  get liveRendererCount(): number {
    let count = 0;
    for (const entry of this.entries.values()) if (entry.hasLiveRenderer()) count += 1;
    return count;
  }

  /** Idempotent per viewId — repeated invocations return the same entry. */
  acquire(viewId: string): RegistryEntry {
    let entry = this.entries.get(viewId);
    if (!entry) {
      const terminal = this.deps.createTerminal();
      const dom = this.createDom();
      dom.className = "xterm-host";
      // Open once into an off-DOM container; mount() only appends that
      // container, so repeated mount/unmount never calls open() twice.
      terminal.open(dom);
      let openCleanup: (() => void) | null = null;
      try {
        const returned = this.deps.onOpen?.(terminal, dom);
        if (typeof returned === "function") openCleanup = returned;
      } catch {
        // 부착 실패(IME 조정기 등)는 터미널 동작에 치명적이지 않다.
      }
      entry = new RegistryEntryImpl(viewId, terminal, dom, this, this.deps, openCleanup);
      this.entries.set(viewId, entry);
      // 뷰가 만들어지기 전에 지정된 배경 덮어쓰기(앱 복원 순서 대비)가
      // 있으면 생성 직후 적용한다 — 이후 applyGlobalPreferences가 유지한다.
      const override = this.backgroundOverrides.get(viewId);
      if (override !== undefined && terminal.options) {
        terminal.options.theme = withBackground(this.baseTheme ?? terminal.options.theme, override);
      }
    }
    return entry;
  }

  get(viewId: string): RegistryEntry | undefined {
    return this.entries.get(viewId);
  }

  has(viewId: string): boolean {
    return this.entries.has(viewId);
  }

  /** Number of live terminal instances (감사/테스트용). */
  get liveCount(): number {
    return this.entries.size;
  }

  disposeAll(): void {
    for (const entry of [...this.entries.values()]) entry.dispose();
  }

  /** Wire fit results (cols/rows + px) to e.g. the session pipeline. */
  setFitHandler(viewId: string, handler: ((dims: FitDimensions) => void) | null): void {
    this.entries.get(viewId)?.setFitHandler(handler);
  }

  /**
   * 모든 살아 있는 터미널에 환경설정(테마·글꼴·커서·스크롤백)을 적용한다.
   * `fontSize`는 **기본값 자체가 바뀐 경우에만** 넘긴다 — 그 외 호출(테마
   * 전환 포함)이 pane 단위 줌(Ctrl/Cmd ±)을 리셋하지 않는다. 글꼴 패밀리가
   * 바뀌면 메트릭이 달라지므로 refit만 돌린다(크기 숫자는 유지).
   */
  applyGlobalPreferences(prefs: {
    fontSize?: number;
    theme: unknown;
    fontFamily?: string;
    cursorStyle?: "block" | "underline" | "bar";
    scrollback?: number;
  }): void {
    this.baseTheme = prefs.theme;
    for (const entry of this.entries.values()) {
      const options = entry.terminal.options;
      if (!options) continue;
      // pane별 덮어쓰기가 있으면 새 기본 테마 위에 다시 얹는다.
      options.theme = withBackground(prefs.theme, this.backgroundOverrides.get(entry.viewId) ?? null);
      if (prefs.cursorStyle !== undefined) options.cursorStyle = prefs.cursorStyle;
      if (prefs.scrollback !== undefined) options.scrollback = prefs.scrollback;
      if (prefs.fontFamily !== undefined && options.fontFamily !== prefs.fontFamily) {
        options.fontFamily = prefs.fontFamily;
        options.lineHeight = lineHeightForFont(prefs.fontFamily);
        entry.refit();
      }
      if (
        prefs.fontSize !== undefined &&
        options.fontSize !== undefined &&
        options.fontSize !== prefs.fontSize
      ) {
        entry.setFontSize(prefs.fontSize);
      }
    }
  }

  /**
   * view별 배경색 덮어쓰기(pane 단위 배경색 — 에이전트 tint·사용자 지정,
   * paneBackground.ts). 색을 null로 지우면 전역 테마 배경으로 돌아간다.
   * 뷰가 아직 없어도 값을 저장해 acquire 때 적용한다. 커서 반대색
   * (cursorAccent)도 같은 색으로 맞춘다 — 블록 커서가 어느 배경에서도
   * 자연스럽게 보이게(내장 테마가 background와 같은 값을 쓰는 것과 같은 규칙).
   */
  setBackgroundOverride(viewId: string, color: string | null): void {
    if (color === null) this.backgroundOverrides.delete(viewId);
    else this.backgroundOverrides.set(viewId, color);
    const options = this.entries.get(viewId)?.terminal.options;
    if (!options) return;
    options.theme = withBackground(this.baseTheme ?? options.theme, color);
  }

  /** 현재 기본 테마의 배경색(없으면 null) — tint 혼합의 베이스 계산용. */
  themeBackground(): string | null {
    const theme = this.baseTheme as { background?: unknown } | null;
    return theme && typeof theme.background === "string" ? theme.background : null;
  }

  /** @internal */
  remove(viewId: string): void {
    this.entries.delete(viewId);
    this.backgroundOverrides.delete(viewId);
  }

  private createDom(): RegistryDom {
    if (this.deps.createDom) return this.deps.createDom();
    if (typeof document !== "undefined") {
      return document.createElement("div") as unknown as RegistryDom;
    }
    throw new Error("no DOM factory available outside the browser");
  }
}

/**
 * Simulate React StrictMode's effect double-invocation
 * (mount → cleanup → mount). The runner is intentionally trivial so tests
 * prove the *subject* handles it, not the runner.
 */
export function runStrictModeEffect(setup: () => () => void): () => void {
  const first = setup();
  first();
  return setup();
}
