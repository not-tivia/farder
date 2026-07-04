# Slash Commands — framework + text/api command bots (v1) — design

**Date:** 2026-07-04
**Status:** design (awaiting owner review)
**Context:** first sub-project of "command/slash bots" (see [[project_farder_bots]]). The existing bots are **poll-driven** (passive); this adds the **request-driven** model — a user types `/trigger args` and a bot answers on demand. This sub-project builds the shared command **framework** plus the two cheapest response types (canned **text** and **api** lookup). `/poll` and `/giveaway` are separate follow-on sub-projects that plug into this framework as new command **types**.

## Problem

There is no way to invoke a bot on demand. We want Discord-style slash commands: type `/`, pick a command, get an answer. The framework must be **client-aware** (the client knows the command list, offers autocomplete, and sends a dedicated invocation — not a raw message) because the later interactive commands (`/poll`, `/giveaway`) require it. v1 ships useful commands (dynamic API lookups + canned text) while establishing that substrate.

## What already exists (v1 reuses)

- **`author_name_override`** on messages + `MessageInfo` (+ `#[serde(default)]`) and the client render that shows an app name + badge instead of resolving a member — built for webhooks. Command responses reuse this (no roster member needed).
- **SSRF-guarded fetch + dot-path extract:** `bots::fetch_json(url)` (`ssrf::resolves_to_global` first, http(s), 10s timeout, 256 KiB cap) + `bots::extract_dot_path(&Value, path) -> Option<f64>` — the api command type reuses these verbatim.
- **Bot-authored channel post:** `messages::insert_message(conn, channel_id, author: &PublicKey, content, reply_to)` + `ServerEvent::NewMessage` broadcast to `EventTarget::Subscribers(channel_id)` — the same path ticker/webhook posts use.
- **Owner gating:** `require_base_perm(conn, member, is_owner, permissions::MANAGE_SERVER, ...)`.
- **Rate limiting:** `RateLimiter::new(count, window_secs)` (used for uploads/reactions/presence).
- **Bots config surface:** `BotsTab.tsx` (Add Ticker / Add Custom Monitor forms + the bot list).

## Goals

1. An owner configures **commands** (trigger + description + response). Two response **types**: **text** (fixed message) and **api** (URL template with `{arg}` → fetch → dot-path → formatted).
2. Any member types `/trigger args`, gets autocomplete, and the bot posts the answer publicly in the channel.
3. The raw `/trigger` line is **not** posted as a message (client-aware dispatch).
4. Command responses render as app posts (name + **BOT** badge), not roster members.
5. The registry + dispatch are **type-extensible** so `/poll`/`/giveaway` add new types without reworking parsing/dispatch/config.

## Non-goals (v1)

- **Ephemeral responses** (only-invoker-sees) — public only; ephemeral is a later cross-cutting feature (a message shown to one user, unpersisted for others).
- **Action/interactive types** (`/poll`, `/giveaway`) — separate sub-projects; this spec only ensures the type enum + dispatch can host them.
- **Permission-gated commands, multiple/typed args, arg-value autocomplete, per-command cooldowns** — a single free-text arg; a global per-user command rate limit.
- **Editing a command** — v1 is create + delete (recreate to change).

## Design

### Data model

New `commands` table (server-scoped): `{ id, trigger TEXT UNIQUE, name TEXT, description TEXT, kind TEXT ('text'|'api'), body_text TEXT, url_template TEXT, value_path TEXT, response_template TEXT, unit TEXT, public_key BLOB (generated author key), created_at }`. For `text`, `body_text` is set; for `api`, `url_template`/`value_path` (+ optional `response_template`/`unit`) are set. `public_key` is a generated non-member key (author of the response, mirrors webhooks). `trigger` is lowercased, `[a-z0-9_-]`, unique per server.

Generalize the webhook badge to be data-driven: add nullable `author_badge TEXT` to `messages` (+ `MessageInfo.author_badge: Option<String>`, `#[serde(default)]`). Webhook posts set `author_badge = "WEBHOOK"`; command posts set `"BOT"`. Client render: show `author_badge` as the badge label when present; when a message has `author_name_override` but no `author_badge` (old webhook rows), fall back to "WEBHOOK" so existing messages are unchanged.

### Requests / responses

- `CommandInfo { id: i64, trigger: String, description: String, takes_arg: bool }` — the **safe, non-secret** view (NO url_template/body — those may hold API keys). `takes_arg = (kind == "api")`.
- `ServerRequest::ListCommands {}` → `ServerResponse::Commands { commands: Vec<CommandInfo> }`. **Not** owner-gated — every member needs it for autocomplete. Returns only the safe fields.
- `ServerRequest::AddCommand { name, trigger, description, kind, body_text: Option<String>, url_template: Option<String>, value_path: Option<String>, response_template: Option<String>, unit: Option<String> }` → `Ok`. **Owner-gated.** Validates: trigger 1–32 `[a-z0-9_-]` + unique; name/description lengths; `kind='text'` requires non-empty `body_text`; `kind='api'` requires `url_template` (http(s), contains `{arg}` optional, ≤2048) + non-empty `value_path`.
- `ServerRequest::DeleteCommand { id }` → `Ok`. **Owner-gated.**
- `ServerRequest::RunCommand { trigger, channel_id, args }` → `Ok` on success (response broadcast) or `Error{message}` on failure (shown to the invoker only — **no channel post on failure**, so failures don't clutter). Any member; per-user rate-limited.

### Dispatch (`RunCommand` handler)

1. Per-user rate-limit (a shared `RateLimiter`, e.g. 5 / 10s); over → `Error("slow down")`.
2. Look up the command by `trigger`. Unknown → `Error("unknown command")`.
3. **text:** post `body_text` (author = the command's `public_key`, `author_name_override = name`, `author_badge = "BOT"`) → broadcast `NewMessage` to `Subscribers(channel_id)` → `Ok`.
4. **api:** URL-encode `args`, substitute into `{arg}` in `url_template` (if no `{arg}`, ignore args); `fetch_json` (SSRF-guarded) → `extract_dot_path(value_path)`. On `Some(v)`: format `response_template` (`{arg}` → the raw arg, `{value}` → the number, thousands-formatted; default template `"{value}"`) + unit, post as in step 3 → `Ok`. On fetch/extract failure → `Error("couldn't run /trigger")` (no post).

`insert_message` must accept the author_name_override + author_badge (extend the webhook helper to `insert_message_with_author(conn, channel_id, author, content, reply_to, name_override, badge)`; webhook delivery passes `("WEBHOOK")`).

### Client — dispatch + autocomplete

- On connect (and after a command is added/deleted), the client calls `listCommands(serverId)` and holds the list.
- **`MessageInput.tsx`:** when the draft starts with `/`, show an **autocomplete menu** of commands whose trigger prefix-matches the first token (trigger + description). On submit of a `/word args` draft: if `word` matches a known command → `runCommand(serverId, word, channelId, args)` (do NOT `sendMessage`; clear the input); the response arrives via the normal `NewMessage` listener. If `word` is unknown → send as a normal message (so non-command slashes still work). A `RunCommand` `Error` is shown to the invoker (reuse the input error affordance) — no channel post.
- **`BotsTab.tsx`:** an **"Add Command"** form (owner-gated): name, trigger, description, kind (text/api), and the kind-specific fields (text: body; api: url template w/ `{arg}` hint, value path, optional response template + unit). A **Commands list** with delete. Refetch `listCommands` after add/delete.

### Security

- `url_template` is **owner-configured** (trusted); the user **arg is URL-encoded** before substitution, so it cannot alter the URL's host/structure — it lands inside the component the owner placed `{arg}` in. `fetch_json`'s `resolves_to_global` still guards the resolved (owner-defined) host. A command can only ever hit the owner's configured endpoint.
- `ListCommands` exposes **no** `url_template`/`body_text` (an api key in a URL stays server-side); only trigger/description/takes_arg reach members.
- Per-user command rate limit bounds outbound fetch spam. Owner-only create/delete; any member runs. `url_template` never logged.

## Testing

- **Dispatch (unit):** text command posts `body_text` with `author_name_override`+`author_badge="BOT"`; api command substitutes a URL-encoded arg, extracts, formats the response template + unit, posts; unknown trigger → `Error`; api fetch/extract failure → `Error` (no message inserted); rate-limit trips → `Error`. (Inject DB/state; api fetch itself is network — assert the URL built from the template + encoded arg via a pure `build_command_url(template, args)` helper, unit-tested; don't hit the network.)
- **`build_command_url` (unit):** `{arg}` substituted + URL-encoded (`a b&c` → `a%20b%26c`); no `{arg}` → template unchanged; response-template format (`{arg}`/`{value}`).
- **CRUD (unit):** `AddCommand` owner-gated + validation (bad trigger chars, dup trigger, api missing url, text missing body); `ListCommands` returns only safe fields (no url_template); `DeleteCommand`.
- **Client:** `cargo build` + `tsc`. Runtime (the `/` menu, run, response) is the owner's Windows test.

## Owner runtime verification (server changed → sidecar rebuild)

Bots → Add Command: (1) a **text** command `rules` → body text; type `/rules` → the bot posts the text (BOT badge), and `/rules` itself doesn't appear. (2) an **api** command `stars` → `https://api.github.com/repos/{arg}`, value path `stargazers_count` (a **static** path — the arg goes in the URL, not the path, which is what the dot-path supports), response template `{arg}: {value} stars`; type `/stars rust-lang/rust` → the bot posts the star count. Typing `/` shows the autocomplete menu. An unknown `/foo` sends as a normal message. A failing api command (bad repo) shows an error to you only, no channel post.

## Decomposition (for the plan)

1. **Server: model + dispatch + CRUD.** `commands` table + `author_badge` (messages/MessageInfo + `insert_message_with_author`, retrofit webhook delivery to pass "WEBHOOK") + `CommandInfo` + `ListCommands`/`AddCommand`/`DeleteCommand`/`RunCommand` requests & handlers + `build_command_url` + text/api dispatch + per-user rate limit. (Unit-tested.)
2. **Client: run path.** Tauri commands (`list_commands`/`add_command`/`delete_command`/`run_command`) + bridge + `CommandInfo` type + `author_badge` type + `MessageInput` `/` interception → `runCommand` (match trigger, unknown → normal send; show RunCommand errors) + the badge render generalization.
3. **Client: autocomplete + config UI.** The `/` autocomplete menu in `MessageInput`; the BotsTab "Add Command" form + Commands list/delete.
4. **Docs.**

## Carry-forward / known limitations

- Dot-path can't index a dynamic key or an array (v1 monitor-bot limitation carried over) — api commands need an endpoint with a static value path.
- Single free-text arg; no typed/multiple args; no per-command permissions or cooldowns.
- Ephemeral responses, editing a command, and the interactive types (`/poll`, `/giveaway`) are follow-ons; the `kind` enum + `RunCommand` dispatch are built to host them.
