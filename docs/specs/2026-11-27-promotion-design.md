# Promotion: a backup copy becomes the host

Status: DESIGN (2026-11-27). Roadmap step 4 in `docs/mesh-hosting-status.md`.

## The problem

A read-only copy (`farder-replica` + `farder-server --read-only`) keeps a server's history readable while the host's PC is off.
Steps 1 to 3 (log fetch, verified copies, client failover) are built and verified.

But if the host is gone for days, or for good, nobody can write. The group needs the copy to **become the host**, without ever
having two machines accepting writes at once. Two writers would fork the signed log, and merging forks is the hard mesh
problem, which this step deliberately avoids.

## The idea: hosting terms, signed by the owner

A **hosting term** is a small record signed by the server's owner. It says "from term N on, machine K is the host, continuing
after event H":

```
HostTerm {
    server_id,          // genesis hash
    term: u64,          // 0 = the original host (implicit, never signed); each promotion is +1
    host_key,           // the new host's server identity (the key its TLS certificate is pinned to)
    addresses: Vec<String>,
    after_event: EventHash,   // the newest event the new host holds; it continues from here
    issued_at: u64,
    signature,          // by the owner identity in genesis
}
```

Rules:
1. **Who signs.** The genesis owner (or the current owner after a recovery-key takeover; the recovery flow already exists).
   If the owner's only device died with the host, the 24-word recovery key gets them back first. Nobody else can promote.
2. **Accepting a term.** A read-only copy accepts a term only if:
   - the signature verifies against the owner key;
   - `server_id` matches;
   - `host_key` is the copy's own key;
   - `after_event` is the head of the copy's own log.

   It then switches to writable at runtime. There is no restart and no file shuffling: the owner's app sends the term to the
   copy over its normal connection (`PromoteToHost { term }`).
3. **Fencing is client-side.** Every app remembers, per server, the highest term it has seen. It:
   - reads `GetHostTerm` on connect;
   - writes only to the host of the highest term it knows;
   - treats a host announcing a LOWER term as a saved copy (read-only, with a banner: "this server moved to <name>");
   - learns new terms from any host or copy it talks to (terms are signed, so anyone can relay them).

   This is what stops the old host, if its PC comes back, from collecting writes. It doesn't know it was replaced, but no
   app that has seen the newer term will write to it.
4. **The old host learns too.** When it starts, it asks the addresses in its own list for their term (best effort). If it
   sees a higher term, it switches itself to read-only and says so in its log. It may then follow the new host as a copy;
   that is the same replica code with the roles swapped (a later step).

## What is lost, and how the app says so

Messages the old host accepted after the copy's last pull are not in the copy. `after_event` makes this exact. The app can
say: "the host changed; anything posted on the old host after <time> was not carried over".

If the old host comes back with those events, they stay on it as read-only history. Merging them in is step 5 (the real mesh)
and is deliberately not attempted here.

## What changes where

| Piece | Change | Testable without a screen |
|---|---|---|
| `farder-crypto` | `HostTerm` + `sign`/`verify` (positional `rmp_serde::to_vec` for the signed bytes, like events) | yes |
| `farder-protocol` | `ServerRequest::{GetHostTerm, PromoteToHost { term }}`, `ServerResponse::HostTerm { term: Option<HostTerm> }` | yes |
| `farder-server` | read-only copy: verify + switch to writable; persist the term; serve `GetHostTerm`; on start, ask peers for a higher term and go read-only if found | yes (real-server tests like `crates/farder-replica/tests/replica.rs`) |
| client crate | per-server highest term; route writes only to its host; lower term = copy; learn terms from anyone | yes (unit) |
| app (UI) | Server Settings > Backup copy > "Make this copy the host" (owner); banner text | no: two-app suite |
| selftest | `failover` gains a `promote` phase: kill the host, owner promotes the copy, chatting continues; restart the old host: apps stay on the new host, and the old host goes read-only | two-app suite |

## Not in this step

- Several writers at once.
- Automatic promotion without the owner (needs trust rules for who may decide).
- Merging the old host's tail.
