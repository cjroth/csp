// WebdriverIO + tauri-driver: drives the REAL Tauri app binary (which
// links native csp-core) through WebKitWebDriver, headless under Xvfb.
// This is the canonical Tauri e2e stack (Playwright cannot drive a Tauri
// binary; it only drives a plain web page).

import { spawn } from "node:child_process";
import path from "node:path";
import process from "node:process";

const APP = path.resolve(
  process.cwd(),
  "src-tauri/target/debug/context-desktop",
);

let tauriDriver;

export const config = {
  runner: "local",
  hostname: "127.0.0.1",
  port: 4444,
  path: "/",
  specs: ["./specs/*.e2e.js"],
  maxInstances: 1,
  capabilities: [
    {
      // tauri-driver reads this and launches the binary via WebKitWebDriver.
      "tauri:options": { application: APP },
    },
  ],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  waitforTimeout: 20000,
  connectionRetryCount: 3,
  mochaOpts: { ui: "bdd", timeout: 60000 },

  onPrepare: () => {
    tauriDriver = spawn("tauri-driver", [], {
      stdio: [null, process.stdout, process.stderr],
    });
  },
  onComplete: () => {
    if (tauriDriver) tauriDriver.kill();
  },
};
