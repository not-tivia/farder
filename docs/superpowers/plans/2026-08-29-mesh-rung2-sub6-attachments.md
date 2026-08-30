# Mesh Rung 2 — Sub-project 6: E2EE attachments + client file policy — Plan

**Goal:** files in an encrypted channel are sealed by the sender and opened only
by members, and the client enforces the file-safety rules the server can no
longer apply to bytes it cannot read.

**Spec:** `docs/superpowers/specs/2026-07-27-mesh-rung2-e2ee-design.md` §"Attachments
in E2EE channels" + coexistence rows 9b and 16. Baseline: `main` @ `2fbb47d`,
932 workspace + 195 client tests green.

## Recon (done first, per the standing lesson)

Checking what previous sub-projects left dormant, because that is where every
bug in this project has come from:

- **`MessageEnvelope` already carries `attachment_keys`, `filenames`, `mimes`**
  (`farder-mls/src/envelope.rs`), unused since sub-1. `send_sealed` hardcodes
  `attachments: vec![]` and a 4a test asserts they are empty. So the transport
  for per-file keys exists and has never been exercised — the exact shape that
  produced the last three sub-projects' bugs.
- **`AttachmentCap { content_hash, declared_type, size, uploader }`** already
  describes a blob by hash+size, which works unchanged over ciphertext.
- **The upload path** (`upload_file_internal_with_channel`) reads the file,
  hashes the PLAINTEXT, guesses MIME from the extension, and streams the bytes.
- **The download path** (`download_file`) returns a data URL for images or
  **saves to disk** — with a server-supplied filename. That write is the hazard
  this sub-project has to close.
- **The server's file hardening** (`image_validation.rs`: magic bytes, formats)
  runs on plaintext bytes and is therefore inoperative for sealed blobs.

## Scope decisions

**D1 — the file policy lives in `farder-crypto`, shared by server and client.**
The spec requires the client to sniff "against the same allowlist version the
server would have applied". Two copies drift, and a drifted allowlist is a
bypass. Both crates already depend on `farder-crypto`, and `media.rs` sets the
precedent for shared server/client helpers living there.

**D2 — sealing uses the existing `farder-crypto` AES-256-GCM primitive** with a
random per-file key, exactly as the spec says. No new construction: file bytes
are not special, and inventing a second AEAD path would be a second thing to get
wrong.

**D3 — the uploaded blob is uniform.** `declared_type = "application/octet-stream"`,
a neutral upload filename, `content_hash = SHA-256(ciphertext)`. The real name and
MIME travel inside the message ciphertext. This also makes ciphertext hashes
uncorrelatable across uploads — strictly better than Rung 1 for the
existence-oracle concern.

**D4 — sanitize on the WAY OUT, not on receipt.** The safe name is computed
immediately before any disk write or render, never stored as if trustworthy. A
sanitized value cached anywhere becomes a value someone later trusts.

**D5 — fail closed on the policy, and say so.** A file whose magic bytes do not
match its claimed type, or whose name cannot be made safe, is NOT written or
rendered; the UI shows why. Silently "cleaning" a hostile file teaches the user
nothing and hides an attack.

## Tasks — 6a (headless, WSL-verifiable)

- [x] **F1 — `farder-crypto::file_policy`.** `safe_filename(raw) -> Result<String>`:
      basename only, reject path separators and traversal, strip bidi/RTL-override
      and control characters, collapse whitespace, enforce a length bound, apply
      the extension allowlist. `sniff(bytes) -> Option<SniffedType>` over the
      magic numbers the server already knows, plus `matches_claim(sniffed, mime)`.
- [x] **F2 — the hostile-input suite (named deliverable).** `../../.ssh/authorized_keys`,
      `C:\Windows\System32\x`, `invoice.pdf.exe`, RTL-override tricks
      (`\u202Egnp.exe`), NUL and control bytes, empty and all-dot names, a 4 KB
      name, a `.png` whose bytes are an ELF, a `.txt` whose bytes are a PNG.
      Each asserts the SPECIFIC refusal, not merely "an error".
- [x] **F3 — seal/unseal a file.** `seal_file(bytes) -> (key, ciphertext)` /
      `open_file(key, ciphertext)` over the existing AEAD. Round-trip, wrong key,
      flipped byte, truncated blob.
- [ ] **F4 — the server keeps working on ciphertext.** A test that cap-vs-blob
      validation (hash, size, uploader) accepts a sealed blob unchanged, and that
      redaction deletes the bytes — the spec's "works unchanged" claims, verified
      rather than assumed.

## Tasks — 6b (wiring + GUI)

- [x] **W1 — sealed upload.** In an E2EE channel, seal before upload and carry the
      key/name/MIME in the envelope. Plaintext channels keep the existing path
      exactly.
- [x] **W2 — `send_sealed` carries attachments** (removing the 4a placeholder and
      the test that asserts they are empty).
- [x] **W3 — sealed download.** Fetch ciphertext, open with the in-envelope key,
      run the F1 policy, and only then render or write. A policy refusal renders
      the reason, never the file.
- [ ] **W4 — voice messages** ride the sealed path (coexistence row 16).
- [ ] **W5 — the harness case:** two clients, one sends a file in an E2EE channel,
      the other opens it; `assert_no_plaintext_anywhere` covers the file bytes AND
      the filename; a non-member fetch gets ciphertext it cannot open.

## Gates
- `cargo test --workspace` ≥ 932; client crate ≥ 195.
- clippy clean on touched crates; `cd client && npx tsc --noEmit`.
- `python3 scripts/seam_audit.py`.
- `git ls-files --eol` after scripted edits.
- Any new UI class present in all three themes.

## Review discipline
Break every load-bearing guard. The load-bearing items: each hostile-filename
rule (F1/F2), the magic-vs-claim check (F1), the seal round-trip (F3), and the
"policy runs before the write" ordering (W3) — which is the one that turns a
sanitizer into a decoration if it lands in the wrong place.

## Carry-forwards
- Attachment count and bucketed sizes still leak; the spec accepts this.
- Sub-7's export/import remains out of scope.
