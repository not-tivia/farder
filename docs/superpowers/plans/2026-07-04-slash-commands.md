# Slash Commands (framework + text/api command bots, v1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Owner-configured slash commands: a member types `/trigger args`, gets autocomplete, and a bot posts the answer (fixed text, or a fetched API value).

**Architecture:** A `commands` registry (trigger + kind text|api + payload). The client fetches the safe command list, intercepts `/` in the message input, and sends `RunCommand` (the raw `/…` never posts). The server dispatches by kind — text posts a fixed body; api substitutes a URL-encoded arg into the owner's URL template, reuses `fetch_json`+`extract_dot_path`, and posts the result. Responses reuse `author_name_override` + a data-driven `author_badge` (no roster member).

**Tech Stack:** Rust (`farder-server`, `farder-protocol`), rusqlite, reqwest+serde_json, percent-encoding, Tauri, React/TS.

## Global Constraints

- **Verify-before-done:** the `/` menu → run → response is the frontend↔backend seam; server changed → sidecar rebuild; runtime is the owner's Windows test.
- **Reuse:** `bots::fetch_json` (SSRF-guarded) + `bots::extract_dot_path`; `messages::insert_message_with_author_name` (extended to carry a badge); the `author_name_override` client render; `require_base_perm(MANAGE_SERVER)`; `RateLimiter`. Do NOT reimplement fetch/extract/auth.
- **Client-aware dispatch:** the raw `/trigger` line is NOT posted; a matched command sends `RunCommand`, an unknown `/foo` sends as a normal message.
- **Response visibility:** public only (post to the channel). Failures return `Error` to the invoker — **no channel post on failure**.
- **Not a roster member:** command responses use `author_name_override` (= command name) + `author_badge = "BOT"`.
- **Security:** `ListCommands` returns only `{id, trigger, description, takes_arg}` — NEVER `url_template`/`body_text` (may hold API keys); the user arg is percent-encoded before substitution (can't alter URL structure); `resolves_to_global` still guards the host; per-user command rate limit; `url_template` never logged. Any member runs; owner-only create/delete.
- **Seam/casing:** new Tauri commands in `generate_handler!`, invoke names match; TS server types snake_case, invoke args camelCase.
- **UI:** any new className added to all three `client/src/themes/*/theme.css`.
- **Build/test:** `cargo test -p farder-server`; `cargo build --workspace`; `cd client/src-tauri && cargo build`; `cd client && npx tsc --noEmit`.

---

### Task 1: Server — schema, data-driven badge, CommandInfo, CRUD

**Files:** `crates/farder-protocol/src/server.rs`; `crates/farder-server/src/db.rs`; `crates/farder-server/src/messages.rs`; `crates/farder-server/src/webhooks.rs`; `crates/farder-server/src/commands.rs` (new); `crates/farder-server/src/handlers.rs`; `crates/farder-server/src/lib.rs`.

**Interfaces:**
- Produces: `MessageInfo.author_badge: Option<String>`; `messages::insert_message_with_author_name(conn, channel_id, author, content, reply_to, name_override: Option<&str>, badge: Option<&str>)` (extended); `CommandInfo { id: i64, trigger: String, description: String, takes_arg: bool }`; `ServerRequest::{ ListCommands{}, AddCommand{...}, DeleteCommand{id} }`; `ServerResponse::Commands { commands: Vec<CommandInfo> }`; `commands::{ CommandRow, create, list_rows, list_infos, find_by_trigger, delete }`.

- [ ] **Step 1: Protocol**

`server.rs`: add `#[serde(default)] pub author_badge: Option<String>` to `MessageInfo` (after `author_name_override`). Add:
```rust
CommandInfo { pub id: i64, pub trigger: String, pub description: String, pub takes_arg: bool }  // derive Serialize/Deserialize/Clone
```
`ServerRequest` variants: `ListCommands {}`; `AddCommand { name: String, trigger: String, description: String, kind: String, body_text: Option<String>, url_template: Option<String>, value_path: Option<String>, response_template: Option<String>, unit: Option<String> }`; `DeleteCommand { id: i64 }`. `ServerResponse`: `Commands { commands: Vec<CommandInfo> }`.

- [ ] **Step 2: Schema**

`db.rs`: guarded `ALTER TABLE messages ADD COLUMN author_badge TEXT`, and:
```rust
        CREATE TABLE IF NOT EXISTS commands (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            trigger           TEXT    NOT NULL UNIQUE,
            name              TEXT    NOT NULL,
            description       TEXT    NOT NULL,
            kind              TEXT    NOT NULL,
            body_text         TEXT,
            url_template      TEXT,
            value_path        TEXT,
            response_template TEXT,
            unit              TEXT,
            public_key        BLOB    NOT NULL,
            created_at        INTEGER NOT NULL
        );
```

- [ ] **Step 3: Badge in messages**

`messages.rs`: add `author_badge: Option<&str>` as the last param of `insert_message_with_author_name`; add `author_badge` to the INSERT column list + params. Add `author_badge` to `MSG_SELECT` (append last so existing indices are stable) and set it in `row_to_message_info`. `webhooks.rs` `deliver`: update the `insert_message_with_author_name(...)` call to pass `Some("WEBHOOK")` for the badge.

- [ ] **Step 4: `commands.rs` CRUD + failing tests**

Create `crates/farder-server/src/commands.rs` (`pub mod commands;` in `lib.rs`). Tests first:
```rust
    #[test]
    fn create_list_find_delete_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        let id = create(&conn, "Rules", "rules", "server rules", "text", Some("be nice"), None, None, None, None).unwrap();
        let infos = list_infos(&conn).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].trigger, "rules");
        assert_eq!(infos[0].takes_arg, false);           // text -> no arg
        let row = find_by_trigger(&conn, "rules").unwrap().unwrap();
        assert_eq!(row.body_text.as_deref(), Some("be nice"));
        // api command -> takes_arg true
        create(&conn, "Stars", "stars", "gh stars", "api", None, Some("https://api.github.com/repos/{arg}"), Some("stargazers_count"), None, Some("stars")).unwrap();
        assert!(list_infos(&conn).unwrap().iter().find(|c| c.trigger == "stars").unwrap().takes_arg);
        delete(&conn, id).unwrap();
        assert!(find_by_trigger(&conn, "rules").unwrap().is_none());
    }
    #[test]
    fn list_infos_excludes_secrets() {
        // CommandInfo has no url_template/body_text fields at all — a compile+shape guard.
        let conn = crate::db::open_in_memory().unwrap();
        create(&conn, "S", "s", "d", "api", None, Some("https://x/{arg}?key=SECRET"), Some("v"), None, None).unwrap();
        let infos = list_infos(&conn).unwrap();
        assert_eq!(infos[0].trigger, "s");               // only safe fields exposed
    }
```
Implementation:
```rust
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use farder_crypto::identity::{Keypair, PublicKey};
use farder_protocol::server::CommandInfo;

pub struct CommandRow {
    pub id: i64, pub trigger: String, pub name: String, pub description: String, pub kind: String,
    pub body_text: Option<String>, pub url_template: Option<String>, pub value_path: Option<String>,
    pub response_template: Option<String>, pub unit: Option<String>, pub public_key: PublicKey,
}

pub fn create(conn: &Connection, name: &str, trigger: &str, description: &str, kind: &str,
    body_text: Option<&str>, url_template: Option<&str>, value_path: Option<&str>,
    response_template: Option<&str>, unit: Option<&str>) -> Result<i64> {
    let pk = Keypair::generate().public_key();
    conn.execute(
        "INSERT INTO commands (trigger, name, description, kind, body_text, url_template, value_path, response_template, unit, public_key, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![trigger, name, description, kind, body_text, url_template, value_path, response_template, unit, pk.as_bytes().as_slice(), crate::db::now() as i64])?;
    Ok(conn.last_insert_rowid())
}
pub fn delete(conn: &Connection, id: i64) -> Result<()> { conn.execute("DELETE FROM commands WHERE id=?1", params![id])?; Ok(()) }
pub fn list_rows(conn: &Connection) -> Result<Vec<CommandRow>> {
    let mut stmt = conn.prepare("SELECT id, trigger, name, description, kind, body_text, url_template, value_path, response_template, unit, public_key FROM commands ORDER BY trigger")?;
    let rows = stmt.query_map([], row_to_command)?; let mut out = Vec::new(); for r in rows { out.push(r?); } Ok(out)
}
pub fn list_infos(conn: &Connection) -> Result<Vec<CommandInfo>> {
    Ok(list_rows(conn)?.into_iter().map(|r| CommandInfo { id: r.id, trigger: r.trigger, description: r.description, takes_arg: r.kind == "api" }).collect())
}
pub fn find_by_trigger(conn: &Connection, trigger: &str) -> Result<Option<CommandRow>> {
    conn.query_row("SELECT id, trigger, name, description, kind, body_text, url_template, value_path, response_template, unit, public_key FROM commands WHERE trigger = ?1", params![trigger], row_to_command).optional().map_err(Into::into)
}
fn row_to_command(r: &rusqlite::Row) -> rusqlite::Result<CommandRow> {
    let pk_b: Vec<u8> = r.get(10)?;
    let arr: [u8;32] = pk_b.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(CommandRow { id: r.get(0)?, trigger: r.get(1)?, name: r.get(2)?, description: r.get(3)?, kind: r.get(4)?,
        body_text: r.get(5)?, url_template: r.get(6)?, value_path: r.get(7)?, response_template: r.get(8)?, unit: r.get(9)?,
        public_key: PublicKey::from_bytes(arr) })
}
```
> Match the real `open_in_memory`/`db::now`/`Keypair`/`PublicKey` names (read `webhooks.rs` — same patterns).

- [ ] **Step 5: CRUD handlers (owner-gated)**

`handlers.rs`: three arms. `ListCommands` is **NOT** owner-gated:
```rust
        ServerRequest::ListCommands {} => ok(ServerResponse::Commands { commands: crate::commands::list_infos(conn)? }),
        ServerRequest::DeleteCommand { id } => {
            if let Some(d) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? { return Ok(d); }
            crate::commands::delete(conn, id)?; ok(ServerResponse::Ok)
        }
        ServerRequest::AddCommand { name, trigger, description, kind, body_text, url_template, value_path, response_template, unit } => {
            if let Some(d) = require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, "MANAGE_SERVER")? { return Ok(d); }
            let name = name.trim().to_string();
            let trigger = trigger.trim().to_lowercase();
            if name.is_empty() || name.len() > 48 { return err("command name must be 1-48 chars"); }
            if trigger.is_empty() || trigger.len() > 32 || !trigger.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                return err("trigger must be 1-32 chars of a-z 0-9 _ -");
            }
            if description.len() > 160 { return err("description too long"); }
            if crate::commands::find_by_trigger(conn, &trigger)?.is_some() { return err("a command with that trigger already exists"); }
            match kind.as_str() {
                "text" => { if body_text.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) { return err("text command needs body text"); } }
                "api" => {
                    let u = url_template.as_deref().unwrap_or("");
                    if !(u.starts_with("http://") || u.starts_with("https://")) || u.len() > 2048 { return err("api command needs an http(s) url template (<=2048)"); }
                    if value_path.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) { return err("api command needs a value path"); }
                }
                _ => return err("kind must be 'text' or 'api'"),
            }
            crate::commands::create(conn, &name, &trigger, description.trim(), &kind,
                body_text.as_deref(), url_template.as_deref(), value_path.as_deref(), response_template.as_deref(), unit.as_deref())?;
            ok(ServerResponse::Ok)
        }
```
> Match the real `ok`/`err`/`require_base_perm`/`permissions::MANAGE_SERVER` (read the `AddCustomBot` arm).

- [ ] **Step 6: Build + test + commit**

`cargo test -p farder-server commands 2>&1 | tail` (pass); `cargo test -p farder-server 2>&1 | tail` (full pass — the new `MessageInfo` field + `insert_message_with_author_name` arity break call sites; fix all, incl. the webhook caller passing `Some("WEBHOOK")`); `cargo build --workspace 2>&1 | tail`.
```bash
git add crates/farder-protocol/src/server.rs crates/farder-server/src/db.rs crates/farder-server/src/messages.rs crates/farder-server/src/webhooks.rs crates/farder-server/src/commands.rs crates/farder-server/src/handlers.rs crates/farder-server/src/lib.rs
git commit -m "feat(commands): commands table, data-driven author_badge, CommandInfo + CRUD"
```

---

### Task 2: Server — RunCommand dispatch (build_command_url, format_response, rate limit)

**Files:** `crates/farder-server/src/commands.rs`; `crates/farder-server/src/state.rs`; `crates/farder-server/src/handlers.rs`; `crates/farder-server/Cargo.toml`.

**Interfaces:**
- Produces: `commands::build_command_url(template: &str, args: &str) -> String`; `commands::format_response(template: Option<&str>, args: &str, value: f64, unit: Option<&str>) -> String`; `ServerRequest::RunCommand { trigger: String, channel_id: u64, args: String }` handler; `ServerState.command_limiter`.
- Consumes: `bots::fetch_json`, `bots::extract_dot_path`, `messages::insert_message_with_author_name` (badge), `messages::get_message`, `connection::broadcast_event`.

- [ ] **Step 1: Dep + pure helpers with failing tests**

`Cargo.toml`: add `percent-encoding = "2"` (already in-tree via reqwest/url — no new download). Tests:
```rust
    #[test]
    fn build_command_url_encodes_arg_preserving_path() {
        assert_eq!(build_command_url("https://api.github.com/repos/{arg}", "rust-lang/rust"),
                   "https://api.github.com/repos/rust-lang/rust");                 // path chars preserved
        assert_eq!(build_command_url("https://x/s?q={arg}", "a b&c"), "https://x/s?q=a%20b%26c"); // space/& encoded — can't inject a param
        assert_eq!(build_command_url("https://x/static", "ignored"), "https://x/static");         // no {arg} -> unchanged
    }
    #[test]
    fn format_response_fills_placeholders() {
        assert_eq!(format_response(Some("{arg}: {value} stars"), "rust", 12345.0, None), "rust: 12,345 stars");
        assert_eq!(format_response(None, "rust", 42.0, Some("stars")), "42 stars");   // default: value [+ unit]
    }
```
Implementation (in `commands.rs`; reuse `crate::bots::format_thousands` if `pub`, else a local copy — the plan's Task-1 monitor-bots `format_thousands` lives in `bots.rs`; make it `pub(crate)` if needed):
```rust
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};

// Encode URL-structure-breaking chars (space, query/fragment/auth, %, control) but keep
// path-friendly chars (/, -, ., _, ~, alphanumerics) so `owner/repo` path args work.
const ARG_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>')
    .add(b'?').add(b'&').add(b'=').add(b'@').add(b'{').add(b'}').add(b'|').add(b'\\').add(b'^').add(b'`');

pub fn build_command_url(template: &str, args: &str) -> String {
    let encoded = utf8_percent_encode(args.trim(), ARG_ENCODE).to_string();
    template.replace("{arg}", &encoded)
}

pub fn format_response(template: Option<&str>, args: &str, value: f64, unit: Option<&str>) -> String {
    let num = crate::bots::format_thousands(value);
    match template.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.replace("{arg}", args.trim()).replace("{value}", &num),
        None => match unit.filter(|u| !u.is_empty()) { Some(u) => format!("{num} {u}"), None => num },
    }
}
```

Run: `cargo test -p farder-server build_command_url format_response 2>&1 | tail` → PASS.

- [ ] **Step 2: Rate limiter in state**

`state.rs`: add `pub command_limiter: RateLimiter` to `ServerState`; init `command_limiter: RateLimiter::new(5, 10)` (5 runs / 10s per user) in `new`. (Match the real `RateLimiter::new(count, window_secs)` signature used by `upload_limiter`.)

- [ ] **Step 3: RunCommand handler**

`handlers.rs`. `RunCommand` is any-member (no `require_base_perm`) but rate-limited per caller. It performs network + broadcast, so keep the DB lock scoped (no guard across `.await`):
```rust
        ServerRequest::RunCommand { trigger, channel_id, args } => {
            let caller = member.public_key.clone(); // match how other arms read the caller's pk
            if !state.command_limiter.check(caller.as_bytes()) { return ok(ServerResponse::Error { message: "slow down — too many commands".into() }); }
            let cmd = { let conn = state.db.lock().unwrap(); crate::commands::find_by_trigger(&conn, &trigger.trim().to_lowercase())? };
            let Some(cmd) = cmd else { return ok(ServerResponse::Error { message: format!("unknown command /{}", trigger.trim()) }); };
            let content: String = match cmd.kind.as_str() {
                "text" => cmd.body_text.clone().unwrap_or_default(),
                "api" => {
                    let url = crate::commands::build_command_url(cmd.url_template.as_deref().unwrap_or(""), &args);
                    let json = match crate::bots::fetch_json(&url).await { Ok(j) => j, Err(_) => return ok(ServerResponse::Error { message: format!("couldn't run /{}", cmd.trigger) }) };
                    match crate::bots::extract_dot_path(&json, cmd.value_path.as_deref().unwrap_or("")) {
                        Some(v) => crate::commands::format_response(cmd.response_template.as_deref(), &args, v, cmd.unit.as_deref()),
                        None => return ok(ServerResponse::Error { message: format!("couldn't read a value for /{}", cmd.trigger) }),
                    }
                }
                _ => return ok(ServerResponse::Error { message: "command misconfigured".into() }),
            };
            // Post the response authored by the command's key, name+badge override. Lock scoped off the await.
            let message = {
                let conn = state.db.lock().unwrap();
                let mid = crate::messages::insert_message_with_author_name(&conn, channel_id, &cmd.public_key, &content, None, Some(&cmd.name), Some("BOT"))?;
                crate::messages::get_message(&conn, mid, &cmd.public_key)?
            };
            if let Some(message) = message {
                crate::connection::broadcast_event(state, crate::events::EventTarget::Subscribers(channel_id),
                    farder_protocol::server::ServerEvent::NewMessage { message }).await;
            }
            ok(ServerResponse::Ok)
        }
```
> Match the real caller-pk access (`member.public_key` or similar — read a nearby arm), the `RateLimiter::check` method name (read `upload_limiter` usage — it may be `.check()`/`.try_acquire()`), `ok`/`ServerResponse::Error`, and whether the arm has `state` in scope (some handler signatures pass `state: &Arc<ServerState>`; if the arm only has `conn`, follow how `AddCustomBot`/alert arms reach state for broadcasts).

- [ ] **Step 4: Build + test + commit**

`cargo test -p farder-server 2>&1 | tail` (pass, incl. the new helper tests) AND `cargo build --workspace 2>&1 | tail` (clean — new `RunCommand`/`Commands` variants may break exhaustive matches; fix).
```bash
git add crates/farder-server/src/commands.rs crates/farder-server/src/state.rs crates/farder-server/src/handlers.rs crates/farder-server/Cargo.toml
git commit -m "feat(commands): RunCommand dispatch (url build, response format, per-user rate limit)"
```

---

### Task 3: Client — commands API, badge render, `/` interception

**Files:** `client/src-tauri/src/commands.rs` + `main.rs`; `client/src/lib/tauri-bridge.ts`; `client/src/lib/types.ts`; `client/src/components/Message.tsx`; `client/src/components/MessageInput.tsx`.

**Interfaces:**
- Produces: Tauri `list_commands`/`add_command`/`delete_command`/`run_command`; bridge `listCommands`/`addCommand`/`deleteCommand`/`runCommand`; `CommandInfo` TS; `MessageInfo.author_badge`.

- [ ] **Step 1: Tauri commands + bridge + types**

`client/src-tauri/src/commands.rs` (mirror `add_custom_bot`/`list_webhooks`): `list_commands(server_id) -> Vec<CommandInfo>`, `add_command(server_id, name, trigger, description, kind, body_text, url_template, value_path, response_template, unit)` (Options as `Option<String>`), `delete_command(server_id, id)`, `run_command(server_id, trigger, channel_id, args)`. Register all four in `generate_handler!`. `tauri-bridge.ts`: the four bridge fns (camelCase; `run_command` maps `Error` responses to a thrown error the caller can show). `types.ts`: `interface CommandInfo { id: number; trigger: string; description: string; takes_arg: boolean }` and add `author_badge?: string | null` to `MessageInfo`.

- [ ] **Step 2: Badge render generalization**

`Message.tsx` (~392): make the badge data-driven —
```tsx
{message.author_name_override && (
  <span className="message-webhook-badge">{message.author_badge ?? "WEBHOOK"}</span>
)}
```
(Old webhook rows have `author_name_override` but no `author_badge` → still "WEBHOOK"; command posts carry `"BOT"`. Reuse the existing `.message-webhook-badge` CSS — no new class.)

- [ ] **Step 3: `/` interception in the message input**

`MessageInput.tsx`: fetch the command list for the server (a `useEffect` on `serverId` calling `api.listCommands(serverId)` into `const [commands, setCommands] = useState<CommandInfo[]>([])`). In `handleSend`, right after `const text = content.trim();` and the empty/sending guard, before any send logic:
```tsx
    if (text.startsWith("/")) {
      const [word, ...rest] = text.slice(1).split(/\s+/);
      const cmd = commands.find(c => c.trigger === word.toLowerCase());
      if (cmd) {
        setSending(true); setError(null);
        try {
          await api.runCommand(serverId, cmd.trigger, channelId, rest.join(" "));
          setContent("");
        } catch (e) { setError(String(e)); }        // RunCommand Error shown to invoker; no channel post
        finally { setSending(false); }
        return;
      }
      // unknown /word -> fall through and send as a normal message
    }
```

- [ ] **Step 4: Build + seam + tsc + commit**

`cargo build --workspace`; `cd client/src-tauri && cargo build`; `grep -n 'run_command\|list_commands\|add_command\|delete_command' client/src-tauri/src/main.rs` (4 registered); `cd client && npx tsc --noEmit`.
```bash
git add client/src-tauri/src/commands.rs client/src-tauri/src/main.rs client/src/lib/tauri-bridge.ts client/src/lib/types.ts client/src/components/Message.tsx client/src/components/MessageInput.tsx
git commit -m "feat(commands): client run path + data-driven badge + slash interception"
```

---

### Task 4: Client — `/` autocomplete menu + Add Command config UI

**Files:** `client/src/components/MessageInput.tsx`; `client/src/components/BotsTab.tsx`; `client/src/themes/*/theme.css`.

- [ ] **Step 1: Autocomplete menu**

`MessageInput.tsx`: when `content` starts with `/` and has no space yet (still typing the trigger), render a small menu above the input listing commands whose `trigger` starts with the typed prefix — each row shows `/trigger` + `description`. Clicking a row sets `content` to `"/" + trigger + " "` and focuses the textarea. Reuse the existing mention-popup pattern in this file (there's an `insertMention`/mention menu — mirror its positioning + `.mention-*` or a new `.command-menu`/`.command-menu-item` class). If a new class is added, style it in all three themes (Step 3).

- [ ] **Step 2: BotsTab Add Command form + list**

`BotsTab.tsx` (mirror the Add Custom Monitor section): a `listCommands(serverId)` fetch on mount; an **"Add Command"** form (owner-gated, same gate the other add-forms use): name, trigger, description, a kind selector (`text`/`api`); for `text` a body textarea; for `api` url template (placeholder `https://api.example.com/{arg}`), value path, optional response template (placeholder `{arg}: {value}`), optional unit. Submit → `api.addCommand(serverId, name, trigger, description, kind, kind==='text'?body:null, kind==='api'?url:null, kind==='api'?path:null, respTemplate||null, unit||null)`; refetch; show errors via the existing `error-text`. A **Commands list** (trigger + description) with a Delete button → `api.deleteCommand(serverId, id)` → refetch. Reuse `connect-*`/`xp-button`/`organizer-*` classes.

- [ ] **Step 3: Theme CSS (if a new class was added) + verify**

If Step 1 added a `.command-menu*` class, add it to all three `client/src/themes/*/theme.css` (mirror the mention-popup styling, theme vars). `cd client && npx tsc --noEmit`; if a new class: `grep -l "command-menu" client/src/themes/*/theme.css` (all three).
```bash
git add client/src/components/MessageInput.tsx client/src/components/BotsTab.tsx client/src/themes/
git commit -m "feat(commands): slash autocomplete menu + Add Command config UI"
```

---

### Task 5: Docs

- [ ] Update `docs/modules/tauri-commands.md` (4 commands), the bridge doc (`CommandInfo`, `MessageInfo.author_badge`, the 4 bridge fns), `docs/modules/protocol.md` (`CommandInfo`, `ListCommands`/`AddCommand`/`DeleteCommand`/`RunCommand`, `Commands` response, the `commands` table + `messages.author_badge`), a server doc (the `commands` module: CRUD, `build_command_url`/`format_response`, RunCommand dispatch text/api, the per-user rate limit, `author_badge` generalization), and `ARCHITECTURE.md` (the slash-command data path: `/` intercept → `RunCommand` → dispatch → bot-authored post; note commands are not roster members and the `kind` enum is the extension point for `/poll`/`/giveaway`). Commit `docs(commands): slash command framework, dispatch, CRUD`.

---

## Owner runtime verification (server changed → sidecar rebuild)

`git pull` → `cargo build -p farder-server` → sidecar copy → `npm run tauri dev` → Ctrl+Shift+R. Bots → Add Command: a **text** `rules` (body text) and an **api** `stars` (`https://api.github.com/repos/{arg}`, path `stargazers_count`, response `{arg}: {value} stars`). In a channel: typing `/` shows the menu; `/rules` posts the text (BOT badge) and the raw `/rules` doesn't appear; `/stars rust-lang/rust` posts the star count; an unknown `/foo bar` sends as a normal message; a bad `/stars nope/nope` shows an error to you only (no channel post); spamming a command trips the rate limit. Confirm an existing webhook post still shows the WEBHOOK badge.

## Self-review notes

- Spec "command = trigger + text|api, not a roster member, author_name_override + BOT badge" → Task 1 (schema/CRUD, badge) + Task 2 (dispatch post).
- Spec "client-aware: ListCommands safe fields, `/` intercept, RunCommand, unknown → normal msg" → Task 1 (ListCommands) + Task 3 (intercept) + Task 4 (menu).
- Spec "api: URL-encode arg into template, SSRF fetch, dot-path, format; failure → Error no post" → Task 2 (build_command_url/format_response/dispatch).
- Spec "author_badge data-driven, webhooks stay WEBHOOK" → Task 1 (column/insert) + Task 3 (render fallback).
- Spec "owner-gated create/delete, any member runs, per-user rate limit, url_template never exposed/logged" → Task 1 (gates + list_infos safe) + Task 2 (rate limit).
- Spec "type-extensible for /poll,/giveaway" → the `kind` string + dispatch match; documented in Task 5.
- Deferred (no tasks): ephemeral, action types, per-command permissions, editing, multi-arg. Correct.
