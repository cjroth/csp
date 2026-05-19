// Tauri IPC bindings. The app always runs inside the Tauri shell now
// (the real csp-core backend); there is no browser/mock path.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(cmd, args);
}

export async function tauriListen<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  return listen<T>(event, (e) => cb(e.payload));
}
