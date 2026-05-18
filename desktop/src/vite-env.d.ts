/// <reference types="vite/client" />

interface Window {
  /** Set by Tauri v2 at runtime; absent in a plain browser. */
  isTauri?: boolean;
  /** Dev-only hook exposed by the TS mock to fire a fake TOFU request. */
  mockTriggerTofu?: () => void;
  /** Dev-only hook exposed by the TS mock to fire a fake superseded-edit. */
  mockTriggerSuperseded?: () => void;
}
