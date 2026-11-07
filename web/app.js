// Synth Remote — web/mobile client for the self-hosted remote-control server.
//
// Speaks the viewer half of Warp's session-sharing protocol (JSON over a websocket) and renders
// the shared terminal with xterm.js. Input goes back as `ExecuteCommand` at an idle prompt and
// as raw `WriteToPty` bytes while a program (shell job, TUI, CLI agent) is running.
"use strict";

const $ = (id) => document.getElementById(id);
const encoder = new TextEncoder();
const decoder = new TextDecoder();

const KEYS = {
  esc: "\x1b",
  tab: "\t",
  "ctrl-c": "\x03",
  "ctrl-d": "\x04",
  up: "\x1b[A",
  down: "\x1b[B",
  right: "\x1b[C",
  left: "\x1b[D",
  enter: "\r",
};

function storageGet(key, fallback) {
  try {
    const value = localStorage.getItem(key);
    return value === null ? fallback : value;
  } catch {
    return fallback;
  }
}

function storageSet(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private mode or blocked storage: the preference just doesn't persist.
  }
}

function show(viewId) {
  for (const id of ["login-view", "list-view", "terminal-view"]) {
    $(id).hidden = id !== viewId;
  }
  $("logout").hidden = viewId !== "list-view";
}

function relativeTime(unixSecs) {
  const delta = Math.max(0, Date.now() / 1000 - unixSecs);
  if (delta < 45) return "just now";
  if (delta < 3600) return `${Math.round(delta / 60)} min ago`;
  if (delta < 86400) return `${Math.round(delta / 3600)} h ago`;
  return `${Math.round(delta / 86400)} d ago`;
}

// Keeps the end of a long path, which is the part that tells terminals apart.
function truncateStart(text, max) {
  return text.length > max ? `…${text.slice(text.length - max + 1)}` : text;
}

function wsOrigin() {
  return `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}`;
}

async function fetchSessions() {
  const response = await fetch("/api/sessions", { credentials: "same-origin", cache: "no-store" });
  if (response.status === 401) return null;
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

// ---------------------------------------------------------------------------
// Login

function showLogin() {
  show("login-view");
  $("login-token").focus();
}

$("login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  $("login-error").hidden = true;
  const response = await fetch("/api/login", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: $("login-token").value }),
  });
  if (response.ok) {
    location.reload();
  } else {
    $("login-error").hidden = false;
  }
});

$("logout").addEventListener("click", async () => {
  await fetch("/api/logout", { method: "POST", credentials: "same-origin" });
  location.href = "/";
});

// ---------------------------------------------------------------------------
// Terminal list

function renderSessions(sessions) {
  const list = $("session-list");
  list.replaceChildren();
  $("empty-state").hidden = sessions.length > 0;
  $("list-meta").textContent = sessions.length ? `${sessions.length} shared` : "";
  $("server-url").textContent = wsOrigin();

  for (const session of sessions) {
    const card = document.createElement("a");
    card.className = "session-card";
    card.href = `/session/${encodeURIComponent(session.id)}`;
    card.setAttribute("role", "listitem");

    const top = document.createElement("div");
    top.className = "row";
    const host = document.createElement("span");
    host.className = "host";
    host.textContent = session.host || "Terminal";
    const pill = document.createElement("span");
    if (!session.sharer_connected) {
      pill.className = "pill warn";
      pill.textContent = "offline";
    } else if (session.command_running) {
      pill.className = "pill ok";
      pill.textContent = session.is_agent ? "agent" : "running";
    } else {
      pill.className = "pill";
      pill.textContent = "idle";
    }
    top.append(host, pill);

    const pwd = document.createElement("div");
    pwd.className = "pwd";
    pwd.textContent = session.pwd ? truncateStart(session.pwd, 44) : session.id;
    pwd.title = session.pwd || session.id;

    const meta = document.createElement("div");
    meta.className = "meta";
    const viewers = session.viewer_count === 1 ? "1 viewer" : `${session.viewer_count} viewers`;
    meta.textContent = `${session.num_cols}×${session.num_rows} · ${viewers} · active ${relativeTime(session.last_activity)}`;

    card.append(top, pwd, meta);
    list.append(card);
  }
}

async function startList() {
  let timer = null;
  const refresh = async () => {
    try {
      const sessions = await fetchSessions();
      if (sessions === null) {
        clearInterval(timer);
        showLogin();
        return;
      }
      show("list-view");
      renderSessions(sessions);
    } catch (err) {
      $("list-meta").textContent = `Could not reach server (${err.message})`;
    }
  };
  await refresh();
  timer = setInterval(() => {
    if (!document.hidden) refresh();
  }, 3000);
}

// ---------------------------------------------------------------------------
// Remote terminal

class RemoteTerminal {
  constructor(sessionId) {
    this.sessionId = sessionId;
    this.ws = null;
    this.viewerId = null;
    this.lastEventNo = null;
    this.blockId = null;
    this.running = false;
    this.role = null;
    this.ended = false;
    this.writeSeq = 0;
    this.retryDelay = 1000;
    this.reportedSize = "";
    this.pingTimer = null;
    this.reportTimer = null;

    const fontSize = Number(storageGet("rc.fontSize", window.innerWidth < 600 ? "12" : "14"));
    this.term = new window.Terminal({
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
      fontSize,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: false,
      theme: { background: "#0b0d10" },
    });
    this.fit = new window.FitAddon.FitAddon();
    this.term.loadAddon(this.fit);
    this.term.open($("terminal"));
    this.term.onData((data) => this.onTerminalData(data));

    new ResizeObserver(() => this.scheduleSizeReport()).observe($("terminal-wrap"));
    $("font-down").onclick = () => this.setFontSize(this.term.options.fontSize - 1);
    $("font-up").onclick = () => this.setFontSize(this.term.options.fontSize + 1);
    for (const button of $("keybar").querySelectorAll("button")) {
      button.addEventListener("click", (event) => {
        event.preventDefault();
        this.onKey(button.dataset.key);
      });
    }
    $("command-form").addEventListener("submit", (event) => {
      event.preventDefault();
      this.submitCommandBar();
    });
  }

  setFontSize(size) {
    const clamped = Math.min(24, Math.max(8, size));
    this.term.options.fontSize = clamped;
    storageSet("rc.fontSize", String(clamped));
    this.scheduleSizeReport();
  }

  setStatus(text, kind) {
    const pill = $("status-pill");
    pill.hidden = false;
    pill.textContent = text;
    pill.className = `pill ${kind || ""}`;
  }

  setBanner(text, bad) {
    const banner = $("banner");
    banner.hidden = !text;
    banner.textContent = text || "";
    banner.className = bad ? "banner bad" : "banner";
  }

  updateModeUi() {
    const input = $("command-input");
    const canExecute = this.role === "Executor" || this.role === "Full";
    input.disabled = !canExecute;
    $("command-send").disabled = !canExecute;
    if (!canExecute) {
      input.placeholder = "Read-only: this server grants viewers no input";
    } else if (this.running) {
      input.placeholder = "Send a line to the running program";
      $("command-send").textContent = "Send";
    } else {
      input.placeholder = "Run a command";
      $("command-send").textContent = "Run";
    }
    if (!this.ended) {
      this.setStatus(this.running ? "running" : "idle", this.running ? "ok" : "");
    }
  }

  connect() {
    const url = `${wsOrigin()}/sessions/join/${encodeURIComponent(this.sessionId)}?client=web`;
    const ws = new WebSocket(url);
    this.ws = ws;
    this.setStatus(this.viewerId ? "reconnecting" : "connecting", "warn");

    ws.onopen = () => {
      this.send({
        Initialize: {
          viewer_id: this.viewerId,
          user_id: { anonymous_id: this.anonymousId(), firebase_id_token: null },
          last_received_event_no: this.lastEventNo,
          latest_block_id: this.viewerId ? this.blockId : null,
          telemetry_context: null,
          feature_support: {
            supports_agent_view: false,
            supports_full_role: true,
            supports_full_role_for_real: true,
          },
        },
      });
      clearInterval(this.pingTimer);
      this.pingTimer = setInterval(() => this.send({ Ping: { data: [] } }), 25000);
    };
    ws.onmessage = (event) => {
      let message;
      try {
        message = JSON.parse(event.data);
      } catch {
        return;
      }
      this.handle(message);
    };
    ws.onclose = () => {
      clearInterval(this.pingTimer);
      if (this.ended || this.ws !== ws) return;
      this.setStatus("reconnecting", "warn");
      setTimeout(() => this.connect(), this.retryDelay);
      this.retryDelay = Math.min(this.retryDelay * 2, 15000);
    };
  }

  anonymousId() {
    let id = storageGet("rc.anonymousId", "");
    if (!id) {
      id = crypto.randomUUID ? crypto.randomUUID() : String(Math.random()).slice(2);
      storageSet("rc.anonymousId", id);
    }
    return id;
  }

  send(message) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(message));
    }
  }

  // Externally tagged serde enums arrive as either "Variant" or { "Variant": payload }.
  static variant(message) {
    if (typeof message === "string") return [message, null];
    const [name] = Object.keys(message);
    return [name, message[name]];
  }

  handle(message) {
    const [kind, body] = RemoteTerminal.variant(message);
    switch (kind) {
      case "JoinedSuccessfully":
        this.onJoined(body);
        break;
      case "RejoinedSuccessfully":
        this.retryDelay = 1000;
        this.updateRole(body.participant_list);
        this.updateModeUi();
        break;
      case "FailedToJoin": {
        const [reason] = RemoteTerminal.variant(body.reason);
        this.end(reason === "SessionNotFound" ? "This terminal is no longer shared." : `Could not join (${reason}).`);
        break;
      }
      case "SessionEnded": {
        const [reason] = RemoteTerminal.variant(body.reason);
        this.end(reason === "EndedBySharer" ? "Sharing was stopped on the terminal." : `Session ended (${reason}).`);
        break;
      }
      case "OrderedTerminalEvent":
        this.onEvent(body);
        break;
      case "ParticipantListUpdated":
        this.updateRole(body);
        this.updateModeUi();
        break;
      case "ParticipantRoleChanged":
        if (body.participant_id === this.viewerId) {
          this.role = body.role;
          this.updateModeUi();
        }
        break;
      case "CommandExecutionRequestFailed": {
        const [reason] = RemoteTerminal.variant(body.reason);
        this.flash(reason === "StaleBuffer" ? "The terminal was busy — try again." : "You don't have permission to run commands.");
        break;
      }
      case "WriteToPtyRequestFailed": {
        const [reason] = RemoteTerminal.variant(body.reason);
        this.flash(reason === "StaleBuffer" ? "Nothing is running to receive input." : "You don't have permission to type here.");
        break;
      }
      case "ViewerRemoved":
        this.end("You were removed from this session.");
        break;
      default:
        // Presence, prompt, input-CRDT and agent-context updates have no web UI yet.
        break;
    }
  }

  updateRole(list) {
    const me = (list.viewers || []).find((v) => v.info.id === this.viewerId);
    if (me) this.role = me.role;
  }

  onJoined(body) {
    this.retryDelay = 1000;
    this.viewerId = body.viewer_id;
    this.blockId = body.init_block_id;
    this.lastEventNo = null;
    this.running = false;
    this.updateRole(body.participant_list);
    this.setBanner(this.role === "Reader" ? "Watching read-only." : "", false);

    this.term.reset();
    this.term.resize(Math.max(2, body.window_size.num_cols), Math.max(2, body.window_size.num_rows));
    for (const block of body.scrollback.blocks || []) {
      this.writeScrollbackBlock(block);
    }
    this.updateModeUi();
    this.reportedSize = "";
    this.scheduleSizeReport();
  }

  writeScrollbackBlock(block) {
    let serialized;
    try {
      serialized = JSON.parse(decoder.decode(new Uint8Array(block.raw)));
    } catch {
      return;
    }
    const command = serialized.stylized_command || [];
    const output = serialized.stylized_output || [];
    if (command.length) {
      this.term.write("\x1b[2m❯\x1b[0m ");
      this.term.write(withCrlf(command));
      this.term.write("\r\n");
    }
    if (output.length) {
      this.term.write(withCrlf(output));
      this.term.write("\r\n");
    }
  }

  onEvent(event) {
    if (this.lastEventNo !== null && event.event_no <= this.lastEventNo) return;
    this.lastEventNo = event.event_no;
    const [kind, body] = RemoteTerminal.variant(event.event_type);
    switch (kind) {
      case "PtyBytesRead":
        this.term.write(decompressPtyBytes(body.bytes));
        break;
      case "CommandExecutionStarted":
        this.running = true;
        this.updateModeUi();
        break;
      case "CommandExecutionFinished":
        this.running = false;
        this.blockId = body.next_block_id;
        this.updateModeUi();
        break;
      case "Resize":
        // The sharer's PTY size is authoritative; when it honors our reported size the two match.
        this.term.resize(Math.max(2, body.window_size.num_cols), Math.max(2, body.window_size.num_rows));
        break;
      default:
        break;
    }
  }

  scheduleSizeReport() {
    clearTimeout(this.reportTimer);
    this.reportTimer = setTimeout(() => this.reportSize(), 200);
  }

  reportSize() {
    const dims = this.fit.proposeDimensions();
    if (!dims || !Number.isFinite(dims.cols) || !Number.isFinite(dims.rows)) return;
    const size = `${dims.cols}x${dims.rows}`;
    if (size === this.reportedSize || !this.viewerId) return;
    this.reportedSize = size;
    this.send({ ReportTerminalSize: { window_size: { num_rows: dims.rows, num_cols: dims.cols } } });
  }

  writeToPty(text) {
    if (!this.viewerId) return;
    this.send({
      WriteToPty: {
        request_id: { participant_id: this.viewerId, op_no: this.writeSeq++ },
        bytes: Array.from(encoder.encode(text)),
      },
    });
  }

  executeCommand(command) {
    if (!command.trim() || !this.blockId) return;
    this.send({ ExecuteCommand: { buffer_id: this.blockId, command } });
  }

  // Keystrokes typed into the terminal itself.
  onTerminalData(data) {
    if (this.running) {
      this.writeToPty(data);
      return;
    }
    // At an idle prompt, Warp owns the input editor, so the command bar acts as the line buffer.
    const input = $("command-input");
    for (const ch of data) {
      if (ch === "\r") {
        this.submitCommandBar();
      } else if (ch === "\x7f" || ch === "\b") {
        input.value = input.value.slice(0, -1);
      } else if (ch === "\x03") {
        input.value = "";
      } else if (ch >= " ") {
        input.value += ch;
      }
    }
  }

  onKey(key) {
    if (this.running) {
      this.writeToPty(KEYS[key]);
    } else if (key === "enter") {
      this.submitCommandBar();
    } else if (key === "ctrl-c") {
      $("command-input").value = "";
    }
  }

  submitCommandBar() {
    const input = $("command-input");
    const text = input.value;
    if (this.running) {
      this.writeToPty(`${text}\r`);
    } else {
      this.executeCommand(text);
    }
    input.value = "";
  }

  flash(text) {
    this.setBanner(text, false);
    clearTimeout(this.flashTimer);
    this.flashTimer = setTimeout(() => this.setBanner("", false), 4000);
  }

  end(text) {
    this.ended = true;
    this.setStatus("ended", "bad");
    this.setBanner(text, true);
    $("command-input").disabled = true;
    $("command-send").disabled = true;
    if (this.ws) this.ws.close();
  }
}

// Sharers send each PTY batch as an LZ4 block with its decompressed size prepended as a
// little-endian u32 (`lz4_flex::block::compress_prepend_size`).
function decompressPtyBytes(bytes) {
  const src = Uint8Array.from(bytes);
  if (src.length < 4) return new Uint8Array(0);
  const size = (src[0] | (src[1] << 8) | (src[2] << 16) | (src[3] << 24)) >>> 0;
  const dst = new Uint8Array(size);
  let s = 4;
  let d = 0;
  const readLength = (length) => {
    if (length !== 15) return length;
    let byte;
    do {
      byte = src[s++];
      length += byte;
    } while (byte === 255 && s < src.length);
    return length;
  };
  while (s < src.length) {
    const token = src[s++];
    const literals = readLength(token >> 4);
    if (d + literals > size || s + literals > src.length) break;
    dst.set(src.subarray(s, s + literals), d);
    s += literals;
    d += literals;
    if (s >= src.length) break; // The last sequence carries literals only.
    const offset = src[s] | (src[s + 1] << 8);
    s += 2;
    const matchLength = readLength(token & 15) + 4;
    if (offset === 0 || offset > d || d + matchLength > size) break;
    // Byte-by-byte: matches may overlap their own output.
    for (let m = d - offset, end = d + matchLength; d < end; ) dst[d++] = dst[m++];
  }
  return dst.subarray(0, d);
}

// Scrollback blocks store bare `\n` line breaks; xterm needs CRLF to return to column 0.
function withCrlf(bytes) {
  const out = [];
  for (let i = 0; i < bytes.length; i++) {
    if (bytes[i] === 0x0a && (i === 0 || bytes[i - 1] !== 0x0d)) out.push(0x0d);
    out.push(bytes[i]);
  }
  return new Uint8Array(out);
}

async function startTerminal(sessionId) {
  let sessions;
  try {
    sessions = await fetchSessions();
  } catch (err) {
    sessions = [];
  }
  if (sessions === null) {
    showLogin();
    return;
  }
  show("terminal-view");
  for (const id of ["font-down", "font-up"]) $(id).hidden = false;
  const summary = sessions.find((s) => s.id === sessionId);
  const title = $("session-title");
  title.hidden = false;
  title.textContent = summary ? [summary.host, summary.pwd].filter(Boolean).join(" · ") : sessionId;
  document.title = `${summary?.host || "Terminal"} — Synth Remote`;

  const remote = new RemoteTerminal(sessionId);
  remote.connect();
}

// ---------------------------------------------------------------------------

if ("serviceWorker" in navigator && window.isSecureContext) {
  navigator.serviceWorker.register("/sw.js").catch(() => {});
}

const match = location.pathname.match(/^\/session\/([^/]+)$/);
if (match) {
  startTerminal(decodeURIComponent(match[1]));
} else {
  startList();
}
