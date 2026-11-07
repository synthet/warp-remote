// Bugs from docs/reports/remote-control-verification-2026-11-05.md.
const { test, expect, openTerminal, moveMouseOverTerminal } = require("./harness");

const ENABLE_MOUSE_TRACKING = "\x1b[?1003h\x1b[?1006h";

test("smoke: live output reaches the browser", async ({ signedIn: page, server, share }) => {
  const terminal = await share("--output", "scrollback-line");
  await openTerminal(page, server, terminal);
  await expect(page.locator(".xterm-rows")).toContainText("scrollback-line");

  terminal.output("live-line\r\n");
  await expect(page.locator(".xterm-rows")).toContainText("live-line");
});

test("an idle terminal shows idle", async ({ signedIn: page, server, share }) => {
  const terminal = await share();
  await openTerminal(page, server, terminal);
  await expect(page.locator("#status-pill")).toHaveText("idle");
});

test("a command started after sharing shows running", async ({ signedIn: page, server, share }) => {
  const terminal = await share();
  terminal.started();
  await openTerminal(page, server, terminal);
  await expect(page.locator("#status-pill")).toHaveText("running");
});

test("a terminal shared mid-command shows running and takes input", async ({ signedIn: page, server, share }) => {
  const terminal = await share("--running");
  await openTerminal(page, server, terminal);
  await expect(page.locator("#status-pill")).toHaveText("running");

  await page.locator("#terminal").click();
  await page.keyboard.type("yes");
  await expect.poll(() => terminal.pty).toContain("yes");
  await expect(page.locator("#command-input")).toHaveValue("");
});

test("the dashboard shows a terminal shared mid-command as running", async ({ signedIn: page, server, share }) => {
  await share("--running");
  await page.goto(server.url);
  await expect(page.locator("#session-list .pill")).toHaveText("running");
});

test("a mid-command share goes idle when the command finishes", async ({ signedIn: page, server, share }) => {
  const terminal = await share("--running");
  await openTerminal(page, server, terminal);
  await expect(page.locator("#status-pill")).toHaveText("running");

  terminal.finished();
  await expect(page.locator("#status-pill")).toHaveText("idle");
});

test("joining during an alt-screen program shows the alt screen", async ({ signedIn: page, server, share }) => {
  const terminal = await share("--running", "--alt-screen", "--output", "normal-buffer-line");
  await openTerminal(page, server, terminal);
  await expect(page.locator("#status-pill")).toHaveText("running");

  terminal.output("alt-screen-line");
  await expect(page.locator(".xterm-rows")).toContainText("alt-screen-line");
  await expect(page.locator(".xterm-rows")).not.toContainText("normal-buffer-line");
});

test("mouse tracking turned on after a mid-command join reaches the program", async ({ signedIn: page, server, share }) => {
  const terminal = await share("--running");
  await openTerminal(page, server, terminal);
  await expect(page.locator("#status-pill")).toHaveText("running");

  terminal.output(ENABLE_MOUSE_TRACKING);
  await expect.poll(async () => {
    await moveMouseOverTerminal(page);
    return terminal.pty;
  }).toContain("\x1b[<");
  await expect(page.locator("#command-input")).toHaveValue("");
});

test("mouse movement at idle leaves the command bar empty", async ({ signedIn: page, server, share }) => {
  const terminal = await share();
  await openTerminal(page, server, terminal);
  terminal.output(`${ENABLE_MOUSE_TRACKING}ready`);
  await expect(page.locator(".xterm-rows")).toContainText("ready");

  await moveMouseOverTerminal(page);
  await page.waitForTimeout(200);
  await expect(page.locator("#command-input")).toHaveValue("");
  expect(terminal.pty).toBe("");
});

test("tracking left on after a command finishes leaves the command bar empty", async ({ signedIn: page, server, share }) => {
  const terminal = await share();
  terminal.started();
  await openTerminal(page, server, terminal);
  terminal.output(`${ENABLE_MOUSE_TRACKING}ready`);
  await expect(page.locator(".xterm-rows")).toContainText("ready");
  terminal.finished();
  await expect(page.locator("#status-pill")).toHaveText("idle");

  await moveMouseOverTerminal(page);
  await page.waitForTimeout(200);
  await expect(page.locator("#command-input")).toHaveValue("");
  expect(terminal.pty).toBe("");
});

test("typing at idle still fills the command bar", async ({ signedIn: page, server, share }) => {
  const terminal = await share();
  await openTerminal(page, server, terminal);
  await page.locator("#terminal").click();
  await page.keyboard.type("ls");
  await page.keyboard.press("ArrowUp");
  await expect(page.locator("#command-input")).toHaveValue("ls");
});

// No sharer event carries `pwd`, so this needs a client or protocol change.
test.fixme("pwd appears for a terminal shared with no finished blocks", async () => {});
