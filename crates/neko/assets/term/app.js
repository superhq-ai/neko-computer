import { WTerm } from "@wterm/dom";

const FACE = { connecting: "(=^o^=)", up: "(=^..^=)", down: "(=x.x=)" };
const statusEl = document.getElementById("status");
const themeEl = document.getElementById("theme");
const termEl = document.getElementById("terminal");
document.getElementById("host").textContent = location.host;

const settings = {
  get theme() { return localStorage.getItem("neko-term-theme") || "default"; },
  set theme(v) { localStorage.setItem("neko-term-theme", v); },
};

const THEMES = ["theme-light", "theme-solarized-dark", "theme-monokai"];

// WTerm owns classes on this element too (wterm, focused); only ever touch
// the theme classes, never replace className wholesale. The chrome stays
// Metal-glass regardless; themes recolor the grid alone.
function applyTheme(name) {
  for (const t of THEMES) termEl.classList.remove(t);
  if (name !== "default") termEl.classList.add(name);
  themeEl.value = name;
}
applyTheme(settings.theme);
themeEl.onchange = () => { settings.theme = themeEl.value; applyTheme(themeEl.value); };

const term = new WTerm(termEl, {
  cursorBlink: true,
  onData: (data) => send(data),
  onResize: (cols, rows) => control({ type: "resize", rows, cols }),
});

let sock = null;
const encoder = new TextEncoder();

function setStatus(state) {
  statusEl.textContent = FACE[state];
  statusEl.classList.toggle("down", state === "down");
  statusEl.title = state === "down" ? "disconnected, click to reconnect" : state;
}
function send(data) {
  if (sock && sock.readyState === WebSocket.OPEN) sock.send(encoder.encode(data));
}
function control(msg) {
  if (sock && sock.readyState === WebSocket.OPEN) sock.send(JSON.stringify(msg));
}
function connect() {
  setStatus("connecting");
  const proto = location.protocol === "https:" ? "wss" : "ws";
  sock = new WebSocket(
    `${proto}://${location.host}/__neko/term/ws?rows=${term.rows}&cols=${term.cols}`,
  );
  sock.binaryType = "arraybuffer";
  sock.onopen = () => { setStatus("up"); term.focus(); };
  sock.onmessage = (ev) => {
    if (typeof ev.data === "string") {
      const msg = JSON.parse(ev.data);
      if (msg.type === "exit") {
        term.write(`\r\n(=x.x=) shell exited (${msg.code})\r\n`);
      }
      return;
    }
    term.write(new Uint8Array(ev.data));
  };
  sock.onclose = () => setStatus("down");
  sock.onerror = () => setStatus("down");
}
statusEl.onclick = () => {
  if (!sock || sock.readyState === WebSocket.CLOSED) connect();
};

await term.init();
connect();
