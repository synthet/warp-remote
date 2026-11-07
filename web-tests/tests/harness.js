// Runs the real server and the scripted fake terminal for each test, on a spare port with a
// throwaway token, so a test never touches a server already running on this machine.
const { spawn } = require("node:child_process");
const net = require("node:net");
const path = require("node:path");
const readline = require("node:readline");
const base = require("@playwright/test");

const TOKEN = "playwright-token-0123456789";
const repoRoot = path.resolve(__dirname, "../..");
const targetDir = process.env.CARGO_TARGET_DIR || path.join(repoRoot, "target");
const exe = process.platform === "win32" ? ".exe" : "";
const serverBin = path.join(targetDir, "debug", `synth-remote-control-server${exe}`);
const sharerBin = path.join(targetDir, "debug", "examples", `fake_sharer${exe}`);

function spareport() {
  return new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.on("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
}

async function startServer() {
  const port = await spareport();
  const host = `127.0.0.1:${port}`;
  const child = spawn(serverBin, ["--bind", host, "--token", TOKEN], { stdio: "ignore" });
  const deadline = Date.now() + 10_000;
  for (;;) {
    try {
      if ((await fetch(`http://${host}/healthz`)).ok) break;
    } catch {
      // Not listening yet.
    }
    if (Date.now() > deadline) throw new Error(`server did not start: ${serverBin}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return { host, url: `http://${host}`, stop: () => child.kill() };
}

class FakeTerminal {
  constructor(child) {
    this.child = child;
    this.ptyBytes = [];
    this.commands = [];
    this.sessionId = new Promise((resolve) => {
      readline.createInterface({ input: child.stdout }).on("line", (line) => {
        const message = JSON.parse(line);
        if (message.session_id) resolve(message.session_id);
        if (message.pty) this.ptyBytes.push(...message.pty);
        if (message.command !== undefined) this.commands.push(message.command);
      });
    });
  }

  // Text the "PTY" has received so far.
  get pty() {
    return Buffer.from(this.ptyBytes).toString("utf8");
  }

  send(command) {
    this.child.stdin.write(`${JSON.stringify(command)}\n`);
  }

  output(text) {
    this.send({ op: "output", text });
  }

  started() {
    this.send({ op: "started" });
  }

  finished() {
    this.send({ op: "finished" });
  }

  stop() {
    this.child.kill();
  }
}

const test = base.test.extend({
  server: async ({}, use) => {
    const server = await startServer();
    await use(server);
    server.stop();
  },

  // Shares a fake terminal. Arguments are `fake_sharer` options.
  share: async ({ server }, use) => {
    const terminals = [];
    await use(async (...args) => {
      const child = spawn(sharerBin, [server.host, TOKEN, ...args], {
        stdio: ["pipe", "pipe", "inherit"],
      });
      const terminal = new FakeTerminal(child);
      terminals.push(terminal);
      await terminal.sessionId;
      return terminal;
    });
    for (const terminal of terminals) terminal.stop();
  },

  // Signs in through the real form, then opens a page on the server.
  signedIn: async ({ page, server }, use) => {
    await page.goto(server.url);
    await page.locator("#login-token").fill(TOKEN);
    await page.locator("#login-form button[type=submit]").click();
    await base.expect(page.locator("#list-view")).toBeVisible();
    await use(page);
  },
});

async function openTerminal(page, server, terminal) {
  await page.goto(`${server.url}/session/${await terminal.sessionId}`);
  await base.expect(page.locator("#status-pill")).toBeVisible();
}

// Moves the pointer across the terminal, which makes xterm report motion if tracking is on.
async function moveMouseOverTerminal(page) {
  const box = await page.locator("#terminal").boundingBox();
  for (const step of [0.2, 0.4, 0.6]) {
    await page.mouse.move(box.x + box.width * step, box.y + box.height * step);
  }
}

module.exports = { test, expect: base.expect, openTerminal, moveMouseOverTerminal };
