/**
 * TerminalView: mounts the registry-owned terminal into this pane's DOM.
 *
 * Lifecycle contract (04-ui.md §4, U09): the effect only acquires/mounts and
 * unmounts DOM+subscriptions. Session creation/kill never happens here, so
 * React StrictMode's mount→cleanup→mount yields exactly one terminal and
 * zero duplicate launches.
 */

import { useEffect, useLayoutEffect, useRef } from "react";
import { useController } from "../../app/controllerContext";
import "@xterm/xterm/css/xterm.css";

const usePrePaintEffect = typeof window === "undefined" ? useEffect : useLayoutEffect;

export function TerminalView(props: { viewId: string }): JSX.Element {
  const { viewId } = props;
  const controller = useController();
  const hostRef = useRef<HTMLDivElement>(null);

  // A split reparents the existing pane. In the browser, move the registry-
  // owned xterm DOM during the layout phase so an empty host is never painted.
  usePrePaintEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const entry = controller.registry.acquire(viewId);
    entry.mount(host);
    return () => {
      // DOM + subscriptions only — 세션과 xterm 인스턴스는 유지된다.
      entry.unmount();
    };
  }, [viewId, controller]);

  return <div className="terminal-view" ref={hostRef} />;
}
