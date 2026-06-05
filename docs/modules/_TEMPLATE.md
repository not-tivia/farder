<!--
Copy this file to docs/modules/<name>.md and fill it in. One doc per file or
tightly-coupled group of files. The goal: a junior dev (or an AI agent) can
understand what this code does, how to call it, and what it connects to —
WITHOUT reading the implementation. Prose over signatures: say what a function
does, what it returns, and its side effects in plain English.
-->

# [ModuleName]

> **File(s):** `path/to/file.rs` (or `.ts`)
> **Layer:** [Tauri command | Tauri bridge | Voice engine | Server crate | Crypto crate | Protocol | Frontend hook | Frontend context | Frontend component]
> **Last reviewed:** YYYY-MM-DD

## Purpose

One to three plain-English sentences. What problem does this module solve? What
does it OWN (state, I/O, decisions)? What does it deliberately NOT do?

---

## Public interface

### `function_or_command_name(param: Type, ...) -> ReturnType`

**What it does:** one sentence, the happy path.
**Parameters:** `param` — what it represents; constraints/valid range if relevant.
**Returns / emits:** on success — the value returned or event emitted; on error —
the error type/string returned.
**Side effects:** state mutations, disk writes, network I/O, events emitted.
**Connects to:** `OtherModule::other_fn` (why it's called); Tauri event
`"event:name"` (what the listener does with it).

<!-- Repeat the block above for each public fn / command / hook / exported function. -->

---

## State it owns

| Field / variable | Type | What it tracks, when it's mutated |
|---|---|---|
| `field_name` | `Arc<Mutex<T>>` | ... |

## Events emitted

| Event name | Payload shape | Who listens |
|---|---|---|
| `"server:new_message"` | `{ server_id, message }` | `useServerEvents.ts` → `ServerContext` |

## Events / requests consumed

| Event / request | Source | What this module does with it |
|---|---|---|

## Integration map

- **[Module A]** — what this takes from / gives to A, naming the exact fn/type at the seam.

## Known gotchas

Anything a newcomer would spend >30 min figuring out: error-prone patterns,
non-obvious invariants, "this is intentionally weird because…".
