# Invite Embeds + Universal Join Gate + Merged Image Menu — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** One JoinConfirmModal gates every invite path (deep link, paste, and new in-chat invite cards); images get a merged right-click menu.

**Spec:** `docs/superpowers/specs/2026-06-11-invite-embeds-join-gate-design.md` — read it first; it is the authority on behavior.

**Gates (all tasks):** `cd client && npx tsc --noEmit` clean; for tasks adding classes, `grep -l "<class>" client/src/themes/*/theme.css` lists all three. Frontend-only; GUI behavior UNVERIFIED until the user's Windows run. ASCII only (HTML entities ok). Commits end with a blank line then:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: Universal join gate (context refactor + paste path)

**Files:** `client/src/context/ServerContext.tsx`, `client/src/App.tsx`, `client/src/components/ConnectDialog.tsx`, `client/src/components/AddServerModal.tsx`.

- [ ] Read App.tsx's joinConfirm/pendingInvite/deep-link handling, ConnectDialog's join submit (~line 127), and AddServerModal's join flow IN FULL first.
- [ ] `ServerContext.tsx`: add `joinConfirmLink: string | null` to state (initial null), actions `OPEN_JOIN_CONFIRM { link }` / `CLOSE_JOIN_CONFIRM`, reducer cases setting/clearing it.
- [ ] `App.tsx`: delete the local `joinConfirm` useState; the deep-link effect dispatches `OPEN_JOIN_CONFIRM` (pendingInvite queue unchanged); `confirmModal` renders from `state.joinConfirmLink`, onConfirm = `{ const u = link; dispatch CLOSE; void joinFromInvite(u); }`, onCancel = dispatch CLOSE. The `relayed` prop expression is unchanged (applied to the context link).
- [ ] `ConnectDialog.tsx` join branch: after `parseInviteLink` yields an `address` and it is an INVITE (relay-form address, or address + inviteCode) — NOT a setupToken flow — dispatch `OPEN_JOIN_CONFIRM(rawInput)` and close/reset the dialog instead of calling connect. Setup-token and 64-hex flows keep their current direct path.
- [ ] `AddServerModal.tsx` join flow: same gating change.
- [ ] tsc clean. Manually trace (in your report): deep link, pasted farder.gg link, pasted relayd link, setup-token — which gate or connect directly.
- [ ] Commit: "Client: every invite path funnels through the JoinConfirm gate".

### Task 2: Invite embeds in chat

**Files:** new `client/src/components/InviteEmbed.tsx`, `client/src/components/Message.tsx`, all three `client/src/themes/*/theme.css`.

- [ ] `InviteEmbed.tsx`: props `{ link: string }`. Uses `useApp()` dispatch. Computes `relayed` via the `relayd?` regex on `parseInviteLink(link).address ?? ""`. Renders:
```tsx
<div className="invite-embed">
  <div className="invite-embed-title">Server invite</div>
  <div className={`join-relay-note ${relayed ? "relayed" : "direct"}`}>
    <span className="join-relay-badge">{relayed ? "RELAYED" : "DIRECT"}</span>
    <span>{relayed ? "Your IP stays hidden from the host." : "The host can see your IP address."}</span>
  </div>
  <button className="xp-button invite-embed-join" onClick={() => dispatch({ type: "OPEN_JOIN_CONFIRM", serverId: "", payload: { link } })}>Join</button>
</div>
```
(Adapt the dispatch shape to however Task 1 defined the action — read ServerContext. If actions require serverId, follow the existing pattern for app-level actions or make the action serverId-less like `SET_ACTIVE_SERVER` style — match the codebase.)
- [ ] `Message.tsx`: after the attachments render, scan `message.content` with an invite regex:
```ts
const inviteRegex = /(?:https?:\/\/)?farder\.gg\/join\/[A-Za-z0-9_-]+|farder:\/\/[^\s]+/gi;
```
filter matches through `parseInviteLink` (keep only results with an `address`), dedupe, cap 3, render `<InviteEmbed key={i} link={m} />` for each. Do NOT remove the raw link from the text. Skip for deleted messages.
- [ ] Theme CSS (all three files, vars only): `.invite-embed` (bordered panel like `.join-relay-note` but block-level, margin-top 6px, padding 8px, border `var(--xp-border)`, background `var(--xp-panel-bg)`, max-width 360px, border-radius 4px), `.invite-embed-title` (bold, 11px, margin-bottom 4px, color `var(--xp-text-normal)` with fallback in xp-luna-blue), `.invite-embed-join` (margin-top 6px). The badge classes already exist.
- [ ] Gates: tsc; `grep -l "invite-embed" client/src/themes/*/theme.css` = 3 files.
- [ ] Commit: "Client: Farder invite links render as join cards in chat".

### Task 3: Merged image right-click menu

**Files:** `client/src/components/Message.tsx` (both the Message component and AttachmentDisplay live here), theme CSS if `.context-menu-divider` is unstyled.

- [ ] Read the message-level context menu (onContextMenu ~line 267 and the menu it opens) and AttachmentDisplay's menu (~580-605) IN FULL.
- [ ] In the Message component, build `const messageActions = [...]` mirroring exactly the items its own context menu offers for this message (respect the same permission/ownership conditions). Pass it through `renderRemainingAttachments`'s closure into `AttachmentDisplay` as a new optional prop `messageActions?: { label: string; onClick: () => void }[]`.
- [ ] AttachmentDisplay image `onContextMenu`: add `e.stopPropagation()`; when opening via RIGHT-click set a flag so the menu renders image items, then `<div className="context-menu-divider" />`, then the messageActions items (each closes the menu after onClick). LEFT-click menu stays image-only (no divider/extras).
- [ ] If `.context-menu-divider` is not styled in the themes, add it to all three (1px line, `var(--xp-border)`, margin 4px 0).
- [ ] Gates: tsc; divider grep (3 files) if added.
- [ ] Commit: "Client: image right-click shows a merged image + message menu".

### Task 4: Docs + final gates

- [ ] `docs/modules/client-relay.md`: note that ALL invite paths (deep link, paste, in-chat invite cards) open the JoinConfirm gate; invite links in messages render as cards (no metadata fetch — privacy). 
- [ ] Run all gates; confirm `cargo build` for the client crate still clean (no Rust touched — sanity only).
- [ ] Commit: "Docs: invite cards + universal join gate".

## Final verification
- [ ] tsc clean; all theme greps pass; client builds.
- [ ] UNVERIFIED (Windows pull): cards rendering, gate on paste, merged menu.
