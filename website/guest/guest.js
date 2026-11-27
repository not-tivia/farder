// Farder guest page. Everything protocol-shaped (frames, signing, the join events) is done by the SAME
// Rust the app and server use, compiled to WebAssembly (pkg/farder_web*). This file is only the screen.
import init, { GuestKey, encode_request, decode_server_frame } from "./pkg/farder_web.js";

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);

// Where am I? Pretty form: /<server name>/t#<code>. Local/testing form: ?server=<name>#<code>.
function route() {
  const m = location.pathname.match(/^\/([A-Za-z0-9_-]+)\/t\/?$/);
  const name = params.get("server") || (m && m[1]) || "";
  const code = decodeURIComponent((location.hash || "").replace(/^#/, ""));
  return { name, code };
}
const { name: routeName, code: inviteCode } = route();
const gatewayUrl = params.get("gateway") || `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws/${encodeURIComponent(routeName)}`;
const store = {
  get(k) { try { return localStorage.getItem(`farder.guest.${routeName}.${k}`); } catch { return null; } },
  set(k, v) { try { localStorage.setItem(`farder.guest.${routeName}.${k}`, v); } catch { /* private mode: still works for this visit */ } },
};

function problem(title, text) {
  for (const s of ["join", "chat"]) $(s).hidden = true;
  $("problem").hidden = false;
  $("problem-title").textContent = title;
  $("problem-text").textContent = text;
}

const hex = (u8) => Array.from(u8, (b) => b.toString(16).padStart(2, "0")).join("");
const unhex = (s) => Uint8Array.from(s.match(/../g).map((h) => parseInt(h, 16)));
const keyOf = (pk) => (Array.isArray(pk) ? pk : pk.bytes).join(",");

let key, ws, nextId = 10, joined = store.get("joined") === "1";
const pending = new Map();           // request id -> {resolve, reject}
const members = new Map();           // key bytes -> display name
let myKey = "", channelList = [], current = null, subscribed = false;

function send(bytes) { ws.send(bytes); }
function ask(requestJson) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    send(encode_request(id, requestJson));
  });
}
function askBytes(make) {      // a request frame built by wasm with an id we choose
  const id = nextId++;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); send(make(id)); });
}
const body = (r) => { const k = Object.keys(r)[0]; if (k === "Error") throw new Error(r.Error.reason); return r; };

function nameOf(pk) { return members.get(keyOf(pk)) || "someone"; }
function timeOf(ts) { return new Date(ts * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }); }

function addMessage(m) {
  if (m.channel_id !== current) return;
  const el = document.createElement("p");
  el.className = "msg" + (keyOf(m.author) === myKey ? " mine" : "");
  el.dataset.id = String(m.id);
  const who = document.createElement("span"); who.className = "who"; who.textContent = m.author_name_override || nameOf(m.author);
  const when = document.createElement("span"); when.className = "when"; when.textContent = timeOf(m.timestamp);
  const text = document.createElement("div"); text.textContent = m.content;     // text only, never HTML
  el.append(who, when, text);
  const box = $("messages");
  if (box.querySelector(`[data-id="${m.id}"]`)) return;
  const stick = box.scrollTop + box.clientHeight >= box.scrollHeight - 40;
  box.append(el);
  if (stick) box.scrollTop = box.scrollHeight;
}

async function openChannel(id) {
  current = id;
  $("messages").textContent = "";
  for (const b of $("channels").children) b.setAttribute("aria-current", String(Number(b.dataset.id) === id));
  const r = body(await ask(JSON.stringify({ FetchHistory: { channel_id: id, before_id: null, limit: 50 } })));
  for (const m of [...(r.History?.messages || [])].reverse()) addMessage(m);
  $("messages").scrollTop = $("messages").scrollHeight;
}

async function enter() {
  $("status").textContent = "loading...";
  let info = body(await ask('"GetServerInfo"')).ServerInfo;
  // On a server with a signed history, a guest also becomes a member of it (their own key signs that).
  if (info.server_id && !joined) {
    body(await askBytes((id) => key.begin_join(id, info.server_id, Date.now() / 1000)));
    const resolved = body(await ask(JSON.stringify({ ResolveInvite: { code: inviteCode } }))).InviteResolved;
    if (!resolved?.invite_event) throw new Error("this guest link is not valid on this server");
    body(await askBytes((id) => key.finish_join(id, info.server_id, resolved.invite_event, Date.now() / 1000)));
    joined = true; store.set("joined", "1");
  }
  // Until the guest is a member the server shows no channels, so ask again now.
  info = body(await ask('"GetServerInfo"')).ServerInfo;
  $("server-name").textContent = info.name;
  document.title = `${info.name} (guest) - Farder`;
  send(key.profile_request(nextId++, store.get("name") || "Guest"));
  const list = body(await ask('"GetMembers"')).Members?.members || [];
  for (const m of list) members.set(keyOf(m.public_key), m.display_name);
  // Plain text channels only: encrypted channels and voice are the app's job.
  channelList = info.channels.filter((c) => c.channel_type === "Text" || c.channel_type === "Announcement");
  const nav = $("channels");
  nav.textContent = "";
  for (const c of channelList) {
    const b = document.createElement("button");
    b.type = "button"; b.dataset.id = String(c.id); b.textContent = "# " + c.name;
    b.addEventListener("click", () => openChannel(c.id));
    nav.append(b);
  }
  body(await ask(JSON.stringify({ Subscribe: { channel_ids: channelList.map((c) => c.id) } })));
  $("join").hidden = true; $("chat").hidden = false;
  if (channelList[0]) await openChannel(channelList[0].id);
  $("status").textContent = "connected as a guest";
}

function onFrame(bytes, resolveAuth) {
  const frame = JSON.parse(decode_server_frame(bytes));
  if (typeof frame === "string") return;
  const kind = Object.keys(frame)[0];
  if (kind === "Response") {
    const p = pending.get(frame.Response.request_id);
    if (p) { pending.delete(frame.Response.request_id); p.resolve(frame.Response.body); }
  } else if (kind === "Event") {
    const ev = frame.Event, ek = typeof ev === "string" ? ev : Object.keys(ev)[0];
    if (ek === "NewMessage") addMessage(ev.NewMessage.message);
    else if (ek === "MemberJoined") members.set(keyOf(ev.MemberJoined.public_key), ev.MemberJoined.display_name);
  } else if (kind === "AuthError") {
    resolveAuth(new Error(frame.AuthError.reason));
  } else if (kind === "Authenticated") {
    resolveAuth(null);
  }
}

function connect() {
  return new Promise((resolve, reject) => {
    ws = new WebSocket(gatewayUrl);
    ws.binaryType = "arraybuffer";
    let stage = "challenge";
    ws.onerror = () => reject(new Error("Could not reach the server. It may be offline."));
    ws.onclose = () => {
      if (stage !== "open") reject(new Error("The server closed the connection."));
      else { $("status").textContent = "disconnected"; $("text").disabled = true; }
    };
    ws.onmessage = (e) => {
      const bytes = new Uint8Array(e.data);
      if (stage === "challenge") {
        stage = "auth";
        try { send(key.answer_challenge(bytes, inviteCode)); } catch (err) { reject(err); }
        return;
      }
      onFrame(bytes, (err) => { if (err) reject(err); else { stage = "open"; resolve(); } });
    };
  });
}

async function start() {
  if (!routeName) return problem("No server in this link", "This link is missing the server's name. Ask whoever invited you for a new one.");
  await init();
  const saved = store.get("secret");
  key = saved ? GuestKey.from_secret(unhex(saved)) : new GuestKey();
  if (!saved) store.set("secret", hex(key.secret()));
  myKey = Array.from(unhex(key.public_key().slice(3))).join(",");     // "my message" styling
  $("name").value = store.get("name") || "";
  $("join").hidden = false;
  $("name").focus();

  const go = async () => {
    const name = $("name").value.trim() || "Guest";
    store.set("name", name);
    $("go").disabled = true; $("join-error").textContent = "";
    try {
      await connect();
      await enter();
    } catch (err) {
      const msg = String(err && err.message || err);
      $("go").disabled = false;
      if (/expired|turned off|invalid invite|maximum uses|not found/i.test(msg)) problem("This guest link no longer works", "It has expired, been used up, or been turned off. Ask for a new one. (" + msg + ")");
      else $("join-error").textContent = msg;
    }
  };
  $("go").addEventListener("click", go);
  $("name").addEventListener("keydown", (e) => { if (e.key === "Enter") go(); });
  $("composer").addEventListener("submit", async (e) => {
    e.preventDefault();
    const text = $("text").value.trim();
    if (!text || current == null) return;
    $("text").value = "";
    try { body(await ask(JSON.stringify({ SendMessage: { channel_id: current, content: text, reply_to: null, attachment_ids: [] } }))); }
    catch (err) { $("status").textContent = "could not send: " + err.message; }
  });
}
start().catch((err) => problem("Something went wrong", String(err && err.message || err)));
