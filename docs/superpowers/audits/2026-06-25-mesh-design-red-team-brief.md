# Farder Mesh Design — Red-Team Brief

**Purpose:** an independent adversarial review. You are asked to find **weaknesses, attack vectors, flawed assumptions, and failure modes** in (a) the Farder system as it exists and (b) the proposed "mesh hosting" design path — especially ones NOT already listed in §6 below. Be skeptical and concrete. Assume the design is flawed and your job is to prove it. Where an established system (Matrix, MLS/OpenMLS, Signal, Secure Scuttlebutt, Briar, Automerge/Yjs, IPFS/libp2p) solved a sub-problem differently, say so and explain why our approach may be worse.

This brief is self-contained; if you also have the repository, inspect the code to confirm or deepen findings.

---

## 1. What Farder is

A privacy-centric, self-hostable chat app (a "self-hosted Discord"):
- **Identity = an Ed25519 keypair**, stored client-side encrypted (Argon2id + AES-GCM behind a 4-digit PIN; 24-word BIP39 recovery phrase). One identity per client today.
- **Client:** React/TypeScript + Tauri (Rust backend in the desktop app).
- **Server:** a Rust `farder-server` process. Per-server **SQLite** database is the single source of truth (messages, channels, members, roles/permissions, invites, bans, files, reactions). Message IDs = SQLite AUTOINCREMENT; ordering = by that id. Server has **no persistent identity** — it generates a throwaway self-signed TLS cert on each restart. "Owner" = the first member to connect (or a setup token).
- **Relay:** a rendezvous server (one default instance on a VPS) that brokers client↔server connections for **IP-masking** (neither side sees the other's IP). The relay does **not** store, replicate, authenticate, or validate anything — it routes opaque handles. It does **not** validate server ownership: any party can register/claim a 32-byte `server_id`.
- **Transport:** QUIC. Servers run either "direct" (client dials a public IP) or "relayed".
- **Encryption today:** **DMs are end-to-end encrypted** (X25519 ECDH derived from the Ed25519 identities). **Channel/community messages are stored and searched in PLAINTEXT** by the host — confidentiality is "trust the host," which is acceptable only because you self-host.
- **Maturity:** pre-1.0, single non-technical product owner, much of the codebase AI-built; many features are "shipped but not yet runtime-verified."

## 2. The mesh vision

Goal: **servers are hosted collectively by their members as a mesh** — joining a server makes you one of its hosts; the server survives any single host going offline; no central machine. Target **tight-knit groups (5–50) first**, large communities later. Decided: **channel content becomes end-to-end encrypted so even hosts can't read it** (a host is then just a store of sealed encrypted blobs); access is enforced by **key possession**, not by a trusted server; **human moderation** by keyed admins/mods (no server-side automod, since hosts can't read); search becomes client-side; new members can't see pre-join history by default.

### Roadmap (rungs)
1. **Rung 1 (designed, see §3):** re-found server truth as a **signed event log** (still single-host, still plaintext). Tamper-evident.
2. **Rung 2:** channel content E2E encryption (group keys; hosts hold sealed blobs).
3. **Rung 3:** replication + multi-host (failover first, then full member-hosting) — gossip/anti-entropy sync.
4. **Rung 4:** always-on "anchor" nodes (e.g. a VPS holding encrypted blobs) for availability; then large-community scale.

## 3. Rung 1 design (the part being built first)

A "walking skeleton": build the whole spine but prove it on **one action — posting a message** — then convert the rest later.

- **Server genesis/identity:** on creation, a content-addressed `Genesis { version, name, owner_pubkey, created_at, nonce }`; `server_id = hash(Genesis)`. Owner is cryptographically fixed. (Relay binding to this id deferred.)
- **Event:** every action becomes a signed log entry:
  `Event { server_id, author (pubkey), seq (per-author counter), prev (hash of author's previous event), lamport (logical clock = 1 + max seen), timestamp (untrusted, tiebreak), payload, signature }`. `event_hash = hash(Event)`.
- **Per-author hash chain** (seq + prev) → gaps/forks detectable.
- **Ordering:** deterministic total order = sort by `(lamport ASC, author_bytes ASC, event_hash ASC)`. `timestamp` never used for ordering.
- **Author signs, not the host.** The **client** builds + signs each event (tracking its own per-server `next_seq`/`prev`/`lamport`); the server only validates (signature, chain continuity, lamport monotonicity, authorization from the *existing* permission tables, content limits), appends to a new append-only `events` table, updates a **derived** `messages` table (the old table demoted to a rebuildable read-cache), and broadcasts the signed event so peers can verify + advance their clocks.
- **Scope this slice:** only messages flow through the log; channels/roles/members/bans stay in DB tables for now; payloads plaintext (encryption is Rung 2); **fresh-start** (existing servers not migrated).

## 4. Security/trust model claims we are making

- Tamper-evidence: a host cannot forge (no author signature), alter (breaks hash/sig), or silently drop an author's event (gap detectable) — even though it stores plaintext.
- Eventually: hosts can't read content (Rung 2 E2EE); access = key possession; ordering is host-independent and survives replication (Rung 3).

## 5. What we want you to evaluate

Attack each of these and anything we missed:
1. **Cryptographic soundness** of the event/genesis/identity scheme and the eventual group-E2EE plan.
2. **Distributed-systems correctness** of the ordering/consistency model under real replication, churn, partitions, and Byzantine participants.
3. **Privacy** leaks (metadata, the relay, timing, social graph) and whether the "hosts can't read it" guarantee actually holds end-to-end.
4. **Abuse/safety/legal**: spam, ban-evasion, illegal-content hosting on members' machines, "right to be forgotten."
5. **Performance/scale/resource** costs on real devices.
6. **Product/UX/operational** viability and sustainability.
7. **Sequencing risk**: are we building Rung 1 in a way that will force a painful redesign at Rung 2/3?

---

## 6. Weak points WE ALREADY SEE (go beyond these)

We list these so you dig deeper, not so you stop here. For each, tell us if it's worse than we think, and find the ones we haven't named.

### Ordering & consistency
- **Lamport order is arbitrary for concurrent events.** It only encodes causality between events that *reference* each other. Concurrent messages are ordered by an attacker-grindable tiebreak (`author_bytes`, then `event_hash` — a sender can mine content to land a favorable hash). So "message order" is manipulable; there is no trustworthy "what really happened first."
- **No trustworthy wall-clock.** `timestamp` is untrusted and unused for ordering; displayed times can be lied about.
- **Head-of-line blocking on the per-author chain.** A lost early event (seq=5) blocks validation of all later events from that author (seq≥6) until it arrives — possible stalls under replication.
- **Cross-channel clock scope** is under-specified (server-wide lamport vs per-channel) — possible anomalies.

### Identity & equivocation
- **Multi-device with one identity forks the chain.** Today identity is one keypair; if a user runs two devices, both author with the same key and independent `seq` counters → guaranteed fork/equivocation. Single-host serialization hides it; the mesh won't. We have no device-subkey / per-device chain design yet. **We think this is a serious hole.**
- **Equivocation under replication.** A malicious author can sign two different events with the same `seq`/`prev` and show different forks to different peers. Single-host rejects the second; the mesh needs witnessing/transparency to detect it. The chain model is being frozen now without solving this.
- **Sybil & ban-evasion are trivial** — identities are free keypairs; a banned user makes a new key. Invite-gating only slightly raises the bar.
- **Key loss = total loss** of identity and one's authored history.

### Encryption (Rung 2, deferred but load-bearing)
- The hardest unsolved part is **group key management**: distribution + rotation on every join/leave (forward secrecy / post-compromise security), in a setting where you **can't trust a server to hand out keys**. Sender Keys vs MLS/OpenMLS is unchosen. "New members can't read history" is a UX hit. Getting Rung 1's data model wrong could make Rung 2 painful.

### Append-only vs deletion/retention (we think this is a sharp contradiction)
- An **append-only signed, replicated log fights "delete my message," retention limits, and right-to-be-forgotten.** Tombstones hide content in the *view*, but the original (encrypted) event can persist forever on every host. Today's server has `retention_secs`; reconcile that with immutability.

### Mesh hosting (Rung 3/4)
- **Availability floor:** for a 5-person group, often *nobody* is online → the server is effectively down. "Anchor" nodes reintroduce an always-on semi-central store (and a juicy target) — does that undermine the decentralization claim?
- **Replication cost** on phones (bandwidth, battery, storage of everyone's encrypted history); N×N peer connectivity / NAT traversal brokered by the relay at scale.
- **Byzantine membership/permissions:** in a mesh, who can add/remove/ban, and how is that agreed? Forked or equivocated membership/role state → split-brain server. We haven't designed BFT membership.

### Privacy & the relay
- The **relay is a central chokepoint**: availability SPOF, DoS target, and a **metadata** vantage point (who connects to which server, when, how much) even if it can't read content. The privacy story leans on "the relay doesn't log," which is trust, not math.
- **Social-graph / traffic-analysis** leaks are unaddressed.

### Existing-system weaknesses (pre-mesh)
- **Relay doesn't validate server ownership** — anyone can claim a `server_id` (hijack/impersonation vector).
- **Ephemeral server cert / no server identity continuity** — MITM and "is this the same server?" concerns (Rung 1 genesis helps, but the relay binding is deferred).
- **Channel content is plaintext** until Rung 2.
- **Untyped frontend↔backend seam** (string `invoke` names vs Rust handlers) — whole features can be silently dead.
- **Maturity/process risk:** much is AI-built, single non-technical owner, many features unverified at runtime, dependency/supply-chain exposure.

---

## 7. Your task

Produce a prioritized findings report. For each finding: **severity** (critical/high/medium/low), the **concrete failure or attack** (ideally a step-by-step scenario), **which rung/component** it hits, whether it forces a **redesign vs. an addition**, and a **suggested mitigation or alternative** (citing how Matrix/MLS/Signal/SSB/Briar/CRDT systems handle it where relevant). Then call out:
- The **single most likely thing to sink this project**, and why.
- Anything in **§6 that we've *underestimated*** (we think multi-device chain forking and append-only-vs-deletion are sharp — are there sharper?).
- Whether **Rung 1 as specified bakes in a decision that will hurt at Rung 2/3**, and what to change *now* to avoid it.
- Any **simpler architecture** that achieves the same goals (mesh-hosted, host-can't-read, small-groups-first) with less risk — e.g., should we just adopt an existing stack (Matrix + MLS, an SSB-style log, a CRDT library, libp2p) instead of hand-rolling?

Be specific and adversarial. We would rather hear the harshest version now than discover it after building the foundation.
