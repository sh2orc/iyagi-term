/**
 * Tauri IPC seam: the one place that touches @tauri-apps/api.
 *
 * `IpcAdapter` abstracts `invoke` + `Channel` so `RealDaemonClient` and the
 * system probe can run against a fake harness in node unit tests, and so
 * `isTauri()` gates the real transport behind the mock in plain-browser dev.
 */

import { Channel, invoke as tauriInvoke } from "@tauri-apps/api/core";

/**
 * Opaque handle for an incoming-message channel. The tauri adapter returns
 * a `Channel` (serialized by reference when passed inside invoke args); test
 * fakes return their own objects.
 */
export type IpcChannelHandle = unknown;

export interface IpcAdapter {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  /** Create a channel; messages arriving from Rust call `onMessage`. */
  channel<T>(onMessage: (message: T) => void): IpcChannelHandle;
}

/** Production adapter over the @tauri-apps/api globals. */
export const tauriIpcAdapter: IpcAdapter = {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(command, args);
  },
  channel<T>(onMessage: (message: T) => void): IpcChannelHandle {
    const channel = new Channel<T>();
    channel.onmessage = onMessage;
    return channel;
  },
};

/**
 * True only inside a Tauri webview (dev preview in a plain browser must fall
 * back to the in-memory mock client).
 */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
