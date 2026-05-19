// End-to-end against the REAL shipping shell: real Tauri window, real
// WebKit webview, real IPC, real native csp-core engine. If any of the
// stack were stubbed or broken these assertions would fail.

import { $, browser, expect } from "@wdio/globals";

describe("Context Desktop — real app + real csp-core", () => {
  it("renders the shell and the Folders view", async () => {
    const sidebar = await $("aside");
    await sidebar.waitForExist({ timeout: 20000 });
    await expect(sidebar).toHaveTextContaining("Context");

    const h1 = await $("h1");
    await h1.waitForExist();
    await expect(h1).toHaveText("Folders");
  });

  it("loaded the real device identity over IPC (proves engine is live)", async () => {
    // The sidebar footer shows the device key fingerprint, which only
    // appears if `get_identity` round-tripped to native csp-core.
    await browser.waitUntil(
      async () => {
        const txt = await $("aside").getText();
        return /SHA256:/.test(txt);
      },
      {
        timeout: 25000,
        timeoutMsg: "device fingerprint never rendered (engine not live?)",
      },
    );
  });

  it("Settings shows the real ed25519 device key", async () => {
    const settingsLink = await $('aside a[href="#/settings"]');
    await settingsLink.click();
    const code = await $("code");
    await code.waitForExist({ timeout: 15000 });
    await expect(code).toHaveTextContaining("ssh-ed25519 ");
  });

  it("Folders empty-state is truthful (no fabricated vaults)", async () => {
    const home = await $('aside a[href="#/"]');
    await home.click();
    const body = await $("main").getText();
    expect(body).toContain("No folders yet");
  });
});
