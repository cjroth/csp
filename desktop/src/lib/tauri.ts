// Tauri runtime detection + lazily-loaded bindings.
//
// Tauri v2 sets `window.isTauri = true` (we do NOT rely on the removed
// `window.__TAURI__`, which only exists with `withGlobalTauri`). Imports
// are dynamic so a plain browser never evaluates the Tauri modules.

export function isTauri(): boolean {
  return typeof window !== "undefined" && window.isTauri === true;
}

export async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export async function tauriListen<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<T>(event, (e) => cb(e.payload));
  return unlisten;
}
