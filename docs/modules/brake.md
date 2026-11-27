# Module: bulk-damage brake (`crates/farder-server/src/brake.rs`)

> **Last reviewed:** 2026-11-15. See `docs/recovery-owner-design.md`.

## Purpose

If an account is stolen, the thief's first move is to wipe the server. This limits how many DESTRUCTIVE
changes one account can make in a short time, owner included. A real owner rarely deletes more than a few
things in ten minutes; a thief is stopped after a few.

## How it works

`handlers::handle_request` is a thin wrapper: for the requests in `is_destructive` (`DeleteChannel`,
`DeleteCategory`, `DeleteRole`, `KickMember`, `BanMember`, `RemoveBot`, `DeleteWebhook`) it asks
`state.brake.check(actor, now)`; over budget it answers `Error("slow down: ...")` and emits an audit event
`destructive_brake` (broadcast to holders of MANAGE_SERVER). Otherwise it runs `handle_request_inner`, and
only when the answer is not an error does it `record` the change, so a refused request costs nothing.

| Item | Detail |
|---|---|
| Budget | 5 per account per 600 s (`--destructive-budget N`, `0` = off) |
| Storage | In memory only: a thief cannot reset it by restarting the server |
| Scope | Per account (the owner is not exempt); other requests are untouched |
| Not covered | It does not undo anything and does not involve the recovery key. `DeleteChannel` of a channel that does not exist is refused (`Error("that channel no longer exists")`) and, being an error, uses no budget |

## Tested

3 unit tests in `handlers.rs` and `brake.rs` (a deliberate break of the counting was caught); the real stack
via `bash scripts/selftest/ubuntu-suite.sh brake`.
