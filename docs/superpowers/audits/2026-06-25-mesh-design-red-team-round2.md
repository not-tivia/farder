# Farder Mesh Design — Red-Team Brief, Round 2

**This is a second adversarial pass.** Round 1 (your prior review) found real issues; the design was revised in response. Your job now is twofold:
1. **Verify, don't trust:** did the revisions *actually* close your round-1 findings, or just paper over them? Be skeptical — look for hand-waving, half-fixes, and "we'll handle it later" where later is too late.
2. **Attack the new surface:** the revisions added new mechanisms (an in-log authorization state machine, an attachment-capability/redaction model, genesis pinning). Find weaknesses *introduced* by the changes.

## Read these (repo access assumed; all under `/farder`)
- **The revised spec:** `docs/superpowers/specs/2026-06-25-mesh-signed-log-foundation-design.md` (git `d181849`). Read it in full — especially the new sections "Attachments as revocable capabilities", "Server identity pinning", the revised "Message flow" authorization step, and "Changes from the external red-team review".
- **Round-1 brief + your prior findings context:** `docs/superpowers/audits/2026-06-25-mesh-design-red-team-brief.md`.
- **The SSRF fix you prompted (now merged):** `crates/farder-server/src/ssrf.rs` and the rewritten `handle_fetch_url` in `crates/farder-server/src/connection.rs`. Confirm it; mirror is from `crates/farder-relay/src/proxy.rs` / `embed.rs`.

## What changed in response to Round 1
- **#8 replayable authorization →** membership + the permission basis are now **in the signed log from Rung 1**; message-event authorization is a *fold of the prior log*, never out-of-log DB state. (This grew Rung 1's scope — it now includes `MemberJoined`/`MemberRemoved`/`PermissionGranted` events, not just `MessagePosted`.)
- **#7 attachment permanence →** attachments are now **content-addressed revocable capabilities**: the log holds an immutable `AttachmentCap {content_hash, declared_type, size, uploader}` reference; the bytes are a separate GC-able/redactable object governed by mutable `moderation_state` + retention; takedown is a signed `AttachmentRedacted` event (Matrix-style redaction).
- **#5 dedupe →** blob records store a `validation_policy_version`; reuse re-evaluates if policy changed; per-reference records kept distinct from blob records.
- **#9 identity →** client **pins the genesis** (TOFU + hard-warn on change); relay registration to require a **genesis-owner signature**.
- **#2 SSRF → FIXED + MERGED.**
- **#1/#3/#4 file hardening →** split into a separate near-term track (server-side magic-byte sniffing/allowlist, download-filename sanitization, media auto-render limits).
- Recorded as later-rung constraints, NOT solved: group-key mgmt + abuse (the "single sinker"), E2EE "blind distributor", **multi-device-with-one-identity chain forking**, Sybil/ban-evasion, relay metadata. Rung 2 will adopt **OpenMLS**; "adopt Matrix wholesale" rejected (federation-of-servers, not member-mesh).

## Round-2 tasks (be specific and adversarial)

### A. Did the fixes actually land?
- **Replayable authz:** Is it *truly* replayable now, or did we just move the problem? Concretely: the spec keeps a DB "permission read-model … cached for fast checks." Can that cache diverge from the log and be consulted on the authz path? Is the *fold* itself well-defined — e.g., concurrent `PermissionGranted`/`MemberRemoved` events (a grant and a kick racing), or a permission event that arrives out of causal order? Does "membership + permission basis in-log, but richer roles/channel-ACLs/bans deferred to follow-ons" leave a window where message authz still depends on not-yet-in-log state (e.g., channel ACL overrides, bans)? Is deferring **bans** safe, given a ban is an authz-revoking fact?
- **Attachment capabilities:** Does the redaction model actually achieve takedown in a replicated/append-only world, or is it theater? After `AttachmentRedacted`, what stops a malicious host from continuing to serve the bytes it already holds (it's "supposed to" GC them)? Is "honest history says a file was here" + removable bytes coherent with the tamper-evidence claim? Does content-addressing leak (hash = fingerprint of known illegal files; or confirm-a-file-exists oracle)?
- **Genesis pinning / relay binding:** TOFU pins on *first* connect — what about the first connect itself (no prior trust; invite link carries the cert fp today)? Does warn-on-change have a safe UX, or train users to click through? Does the genesis-owner-signature relay binding actually prevent a malicious relay or a malicious co-host (Rung 3) from impersonating/eclipsing? What about owner key rotation/compromise — is the genesis owner forever?
- **SSRF fix:** independently confirm no bypass (redirect re-validation timing, DNS rebinding residual, multi-A-record, IPv6 mapped). The author claims the residuals are disclosed — agree?

### B. New weaknesses introduced by the revisions
- The **in-log authorization state machine** is new attack surface: equivocation on membership/permission events; an owner/admin signing conflicting authz events shown to different hosts; replay/reordering of authz events to grant-then-act or act-before-revoke; can a member forge a `MemberJoined` for themselves (who must sign it — the joiner, the owner, the inviter)? Walk the join/kick/grant flows and their signatures.
- The **AttachmentCap** model: can a sender reference a `content_hash` they never uploaded (claim someone else's blob / a not-yet-existing blob)? Does size/type in the cap get verified against the actual bytes? Mismatch handling?
- The **scope growth** of Rung 1 (now an authz state machine) — does that make Rung 1 too big / risky to land in one piece, and should it be re-split?

### C. Still-baked-in regrets
- Given the changes, does Rung 1 *still* commit to something that hurts at Rung 2 (E2EE) or Rung 3 (replication/multi-host)? Specifically: **multi-device-with-one-identity chain forking** is deferred — but Rung 1 freezes the per-author `seq`/`prev` chain model now. Is deferring it a mistake — does the chain model need to change *now* to admit per-device sub-chains, or is it safely additive later? This is the one we're most worried about.
- Group keys / abuse (the "single sinker") — anything in Rung 1 that should change now to not corner Rung 2's MLS adoption?

### D. Verdict
Prioritized findings (severity, concrete attack/scenario, which fix is incomplete or which new flaw, redesign-vs-addition, mitigation). Then: **the single highest-risk thing remaining**, whether **Rung 1 is now safe to build or still needs a change first**, and whether the **scope growth warrants re-splitting Rung 1**. If the revisions are genuinely sound, say so plainly — but we'd rather hear what's still wrong.
