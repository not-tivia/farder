# Toast notifications

> **File(s):** `client/src/lib/toast.ts`, `client/src/components/ToastContainer.tsx`
> **Layer:** Frontend utility
> **Last reviewed:** 2026-06-04

## Purpose

App-wide, non-intrusive notifications. Replaces `window.alert(...)` and silent
`catch {}` for transient user feedback (a failed action, a saved setting). The
API is callable from **anywhere** — components, hooks, or plain functions — with
no React context, by dispatching a `window` `CustomEvent` (matching the
codebase's existing event pattern). The `<ToastContainer>` mounted at the app
root listens and renders the stack.

## Public interface

### `toast.error(message) / toast.success(message) / toast.info(message)`

**What it does:** shows a toast of the given variant. **Side effects:** dispatches
a `farder:toast` `CustomEvent` (`{ id, message, variant }`). **Connects to:**
`ToastContainer`, which subscribes to that event.

```ts
import { toast } from "../lib/toast";
toast.error(`Couldn't save edit: ${e}`);
```

### `<ToastContainer />`

Mounted once in `App.tsx` (inside `AppProvider`, alongside `<AppInner>`).
Subscribes to `farder:toast`, keeps up to **4** visible toasts, auto-dismisses
each after **4.5s**, renders bottom-right with a manual close button.

## Events consumed

| Event | Source | Effect |
|---|---|---|
| `farder:toast` | `lib/toast.ts` | append to the visible stack; auto-dismiss after 4.5s |

## Integration map

- **`Message.tsx`** — edit/delete/create-thread/reaction failures call `toast.error`.
- **`ChannelSidebar.tsx`** — voice-join failure calls `toast.error`.
- Styling: `.toast-*` classes in each `client/src/themes/*/theme.css`
  (theme-variable-driven, with error/success/info accent colors).

## Known gotchas

- It's fire-and-forget: there's no return value / promise. For high-frequency
  failures (e.g. rapid nav retries on a flaky link) prefer `console.error` to
  avoid stacking toasts; `MAX_VISIBLE` (4) bounds the stack regardless.
