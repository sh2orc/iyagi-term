/**
 * App root: wires the workbench to a daemon client.
 *
 * Inside the Tauri webview the real transport client (bridge RPCs + Channel
 * event/output streams, ticket I04) is used; the plain-browser dev preview
 * keeps MockDaemonClient (echo shell, queue admission, resource snapshots)
 * behind the same DaemonClient interface — no component changes required.
 */

import { useEffect, useState } from "react";
import { homeDir } from "@tauri-apps/api/path";
import { createDaemonClient } from "../features/bridge/realClient";
import { isTauri, tauriIpcAdapter } from "../features/bridge/ipc";
import { mockSystemProbe, tauriSystemProbe } from "../features/bridge/systemProbe";
import { setManagedRunDeps } from "../features/workloads/managedRunDeps";
import { setShellProbe } from "../features/terminal/shellDeps";
import { autoConfigureDetectedProfiles } from "../features/profiles/autoDiscovery";
import { installMockDaemonTestHarness } from "../features/missions/devHarness";
import { Workbench } from "./Workbench";

/**
 * Module-level singleton: StrictMode double-render must not duplicate it.
 * `isTauri()` is checked once at module load; the transport choice does not
 * change during a session.
 */
const client = createDaemonClient();

// Vite can replace this module without destroying the native WebView. Drop
// callbacks held by the superseded client so only the next controller owns
// daemon events; workloads themselves remain in the detached daemon.
if (import.meta.hot) {
  import.meta.hot.dispose(() => client.dispose?.());
}

// 관리 실행 폼(I11)은 DaemonClient와 SystemProbe를 seam으로 주입받는다.
// Tauri 안에서는 Rust 브리지 명령 실행, 브라우저 미리보기는 결정적 mock.
const probe = isTauri() ? tauriSystemProbe(tauriIpcAdapter) : mockSystemProbe();
setManagedRunDeps({ client, probe });
setShellProbe(probe);
if (isTauri()) {
  void autoConfigureDetectedProfiles(probe).catch(() => undefined);
} else if (import.meta.env.DEV) {
  // 브라우저 미리보기 전용 시나리오 패드(O16): window.__mockDaemon 노출.
  // production build에서는 이 분기가 사라진다.
  installMockDaemonTestHarness(client);
}

export function App(): JSX.Element | null {
  const [home, setHome] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    void homeDir().then(
      (path) => { if (active) setHome(path); },
      (cause: unknown) => { if (active) setError(String(cause)); },
    );
    return () => { active = false; };
  }, []);
  if (error) return <p role="alert">{error}</p>;
  // Resolve the native home before Workbench can launch its first session.
  if (isTauri() && home === undefined) return null;
  return <Workbench client={client} home={home} />;
}
