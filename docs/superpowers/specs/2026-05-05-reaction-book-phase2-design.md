# Reaction Book Phase 2 — Design

**Status:** Approved 2026-05-05
**Scope:** Farder client (Tauri + React). No server changes (no protocol additions). Builds on Reaction Book Phase 1.
**Predecessor:** `docs/superpowers/specs/2026-05-04-reaction-book-phase1-design.md`

## Goal

Make the Reaction Book actually useful inside conversations — not just for reactions on hover. Type `:my-cat:` in a message and have it render as the cat image inline. Type `:smile:` and have it render as 😄. Quick-send a book item as a standalone sticker message via a slim picker in the message input. Replace the now-vestigial Favorites system with the book-backed equivalent.

## Non-Goals (Phase 2)

- **Most-used sort + drag-to-reorder** in BookBrowser — deferred to Phase 2.5. Requires `use_count` field on BookItem + per-use increment.
- **GIF search via Tenor/Giphy** — separate queued feature with its own design.
- **Bundled emoji image set (Twemoji etc.)** — Phase 2 uses OS-default emoji rendering. The render path is structured so swapping to a bundled set is a one-function change later (see "End-game emoji rendering" below).
- **Standalone emoji-insert picker** (Discord-style "smiley" button that opens a Unicode emoji picker for inserting characters into text). The `:name:` autocomplete covers the common case; a dedicated picker can be a follow-up if needed.
- **`use_count` tracking** for sort — defer.

## Architecture

### Three pieces, shared infrastructure

All three features (inline render, autocomplete, send-as-sticker) operate on the same book items + the same per-server upload cache from Phase 1. **No protocol changes** — messages already have an `attachments` array; inline emoji ride on it as regular file attachments.

### Render-at-display-time, not as-you-type

The user's typed text stays as plain `:my-cat:` in the input. On send, the message body text is sent as-is. The render step on each receiving client (including the sender) parses the text for `:name:` tokens and replaces them inline at display time. The original text is preserved for edit, search, and copy/paste — same model Discord uses.

### How inline emoji become message attachments

When you send `"hello :my-cat: world"`:

1. Client parses tokens. Finds `:my-cat:`.
2. Resolves the file_id via `book_get_file_for_server(serverId, "my-cat")` (cached after first use).
3. Calls `send_message(serverId, channelId, "hello :my-cat: world", attachmentIds: [<file_id>])`.
4. The message text stays as `"hello :my-cat: world"`.
5. On render, the receiving client matches the token to the attachment and renders the image where the token sits — the attachment is *not* shown a second time below the text.

The server doesn't know about emoji at all. They're regular attachments. Inline-rendering is purely a client concern.

### Render component: `RenderedMessageContent`

A pure render component that takes:
- `text: string` — message body
- `attachments: AttachmentInfo[]` — message attachments
- `bookIndex: BookItem[]` — current user's book (loaded once)

And produces a React fragment alternating text spans, inline emoji images, and inline Unicode glyphs.

Pseudocode:

```ts
function RenderedMessageContent({ text, attachments, bookIndex }) {
  const tokens = parseColonTokens(text);  // [{name: "my-cat", index: 6, length: 8}, ...]
  const usedAttachmentIds = new Set<number>();
  const out: React.ReactNode[] = [];
  let cursor = 0;

  for (const tok of tokens) {
    out.push(<span>{text.slice(cursor, tok.index)}</span>);
    cursor = tok.index + tok.length;

    const bookMatch = bookIndex.find(b => b.name === tok.name);
    if (bookMatch) {
      // Find the attachment uploaded for this book item — match by uploaded filename.
      const att = attachments.find(a =>
        a.original_name === `${tok.name}.${bookMatch.ext}` &&
        !usedAttachmentIds.has(a.file_id)
      );
      if (att) {
        usedAttachmentIds.add(att.file_id);
        out.push(<InlineBookEmoji attachment={att} serverId={serverId} />);
        continue;
      }
    }
    const unicode = lookupShorthand(tok.name);  // returns codepoint string or null
    if (unicode) {
      out.push(renderUnicodeEmoji(unicode));    // see "End-game emoji rendering"
      continue;
    }
    // No match — render as literal text.
    out.push(<span>{text.slice(tok.index, tok.index + tok.length)}</span>);
  }
  out.push(<span>{text.slice(cursor)}</span>);

  return (
    <>
      {out}
      {/* Render any attachments NOT consumed by inline emoji as normal attachments below. */}
      <AttachmentList attachments={attachments.filter(a => !usedAttachmentIds.has(a.file_id))} />
    </>
  );
}
```

### Inline emoji name match by attachment filename

Matching tokens to attachments uses `original_name` (e.g. `my-cat.png`). For this to work reliably, `book_get_file_for_server` must upload book items with `original_name = "<book_name>.<ext>"` rather than the current `<id>.<ext>`. Small change to the existing command — `book.rs` is updated so the upload uses the book item's name as the filename.

If a user sends two `:my-cat:` tokens in one message, both reference the same file_id (one attachment, two render positions). The render code above doesn't fully handle this — it only renders the FIRST occurrence as inline and leaves subsequent ones as text. **Acceptable v1 behavior**; very rare. A follow-up can allow multiple inline references to the same attachment.

### End-game emoji rendering

The function `renderUnicodeEmoji(codepoint: string): React.ReactNode` is the single point of change for cross-platform visual consistency:

- **Today (Phase 2):** returns `<span>{codepoint}</span>` — OS-default rendering. Each user sees their OS's emoji set (Apple on macOS/iOS, Segoe on Windows, Noto on Android, etc).
- **Future:** returns `<img src={`/emoji/${codepointToHex(codepoint)}.svg`} ...>` — bundled Twemoji or equivalent. Every user sees the same visuals.

The function lives in `lib/unicodeEmoji.ts`. Swapping the implementation requires no other code changes.

### Unicode shorthand mapping

The `lib/unicodeEmoji.ts` module wraps a shortcode → codepoint map covering the GitHub/Discord standard set (~1500 entries). Use the npm `node-emoji` package or its data file directly. Lookup is `O(1)` via a Map.

### Autocomplete

`EmojiAutocomplete.tsx` — a small dropdown rendered above the message input when the user is typing a `:name:` token.

**Trigger logic:**
- On every textarea input event, parse the cursor's surrounding text
- If the cursor is inside `:[\w-]+` (open colon followed by 2+ word chars, no closing colon yet), open the dropdown
- Query = the text between `:` and cursor
- Otherwise, close

**Result list:**
- Up to 8 matches
- Book items first (alphabetical by name), then Unicode shorthand matches
- Each row shows the rendered preview + the name (`🐱  my-cat` for book, `😄  smile` for Unicode)
- Arrow up/down navigates, Enter or Tab inserts the full `:name: ` (with trailing space), Esc closes
- Click also inserts

**Position:**
- Floats above (or below if no room above) the textarea, anchored to the cursor's screen position approximated as the textarea's bounding box top-left

### SendStickerPicker (replaces FavoritesPanel)

The existing `*` button in MessageInput is repurposed — instead of opening `FavoritesPanel`, it opens a new `SendStickerPicker`.

**Layout:**
- Slim grid popover above the `*` button
- Header: search input
- Grid: 4-5 columns of small thumbnails (~64x64), name on hover
- Click a thumbnail → resolves file_id via `book_get_file_for_server` → calls `send_message(content: "", attachmentIds: [fileId])` → picker closes
- Empty state: "Your book is empty. Open the 📚 button to add items."

**Behavior:**
- Sort is fixed to "recent" for v1 (no most-used yet — that's Phase 2.5)
- Search filters by name substring, case-insensitive
- The picker closes on outside click, on Esc, and after sending

### What happens to the old FavoritesPanel + commands

- `FavoritesPanel.tsx` component is deleted
- The `*` button still exists in MessageInput but its `onClick` now opens SendStickerPicker
- The Tauri commands `add_favorite` / `list_favorites` / `remove_favorite` stay registered (no harm — they just become dormant). A follow-up release can deregister them.
- `~/.farder/favorites/` directory and `favorites.json.bak` stay on disk (created/preserved by Phase 1's migration). Deleting old user data is out of scope.

## Components summary

**New:**
- `client/src/components/SendStickerPicker.tsx` — slim grid for click-to-send
- `client/src/components/EmojiAutocomplete.tsx` — typing dropdown
- `client/src/components/RenderedMessageContent.tsx` — pure text+attachment renderer
- `client/src/components/InlineBookEmoji.tsx` — small `<img>` wrapper that uses the same imageCache + downloadFile pattern as the existing AttachmentDisplay (~22px inline)
- `client/src/lib/unicodeEmoji.ts` — shortcode → codepoint map + `renderUnicodeEmoji` function (single point of change for end-game rendering)

**Modified:**
- `client/src/components/MessageInput.tsx` — `*` button repurposed; autocomplete state + handler
- `client/src/components/Message.tsx` — switches body render from raw text to `<RenderedMessageContent>`
- `client/src-tauri/src/book.rs` — `book_get_file_for_server` uploads with `original_name = <book_name>.<ext>` instead of `<id>.<ext>` for inline-attachment matching

**Deleted:**
- `client/src/components/FavoritesPanel.tsx`

## Edge cases

- **Edit message** → restores raw `:my-cat:` text into the textarea, NOT the rendered image. Rendering is display-time only.
- **Search messages** → unchanged. Server searches the `content` column which still contains `:my-cat:` literal. So `cat` matches.
- **Sender sees their own messages rendered.** Render path runs for all messages including own.
- **Multiple uses of same token in one message** (e.g. `":cat: :cat:"`). v1 renders only the first as inline (because each attachment is consumed once). Subsequent occurrences fall through to literal text. Acceptable; rare.
- **Backwards compat (old client receiving from new client):** old client renders text literally (`:my-cat:` shows as text) and the attachment as a normal image attachment below the text. Slightly redundant but functional.
- **Forward compat (new client receiving from old client):** old client sends no `:name:` tokens; render pass finds none, falls through to plain text + normal attachments. No regression.
- **Token name collision:** book item named "smile" overrides Unicode `:smile:`. Documented for users in the BookBrowser empty state or tooltip.
- **`SendStickerPicker` empty state:** prompts user to open BookBrowser first.

## Backwards compatibility

No protocol changes. Old clients can connect to new servers and vice versa. Inline emoji are regular attachments; old clients see them as attachments (no inline render).

## Success criteria

- Typing `:my-cat:` (where my-cat is a book item) → on send, message renders with the cat image inline at that position. Sender sees the same render.
- Typing `:smile:` → renders as 😄 inline.
- Typing `:nonsense_word:` → stays as literal text `:nonsense_word:`.
- Typing `:my` and pausing → autocomplete dropdown shows book matches first, Unicode after.
- Arrow keys + Enter in autocomplete → token gets inserted with trailing space.
- Esc dismisses autocomplete; typing continues normally.
- Click `*` button (now SendStickerPicker) → grid opens. Click a thumbnail → message sends with that image as a sticker. Picker closes.
- Old FavoritesPanel — gone. `*` button has new behavior.
- Edit a sent message → source `:my-cat:` text appears in textarea (not rendered image).
- Search for `cat` → matches messages containing `:my-cat:`.
- Old clients on the same server receive new clients' messages as text + attachment (degraded but functional). No errors.
- Switching `renderUnicodeEmoji` from `<span>{cp}</span>` to `<img>` requires editing only `lib/unicodeEmoji.ts`.

## Implementation notes (non-binding, for the planner)

- **The `book_get_file_for_server` change to use book name as filename** is a small but important detail. The current implementation uses `<id>.<ext>` which prevents the inline-emoji name-match. Update the upload command to use `<book_item.name>.<ext>` instead. Existing cached file_ids in `server_files` keep working (server has the file under whatever name it was originally uploaded with) — only NEW uploads get the new naming. After this change ships, items uploaded BEFORE the change still render their inline-emoji form correctly because the match falls through to "leave as literal text" — degraded but not broken.
- **`node-emoji` package** can be a heavy dep (~50KB minified). Consider just embedding its data file directly into a `lib/unicodeEmoji.ts` constant if bundle size matters. The data is JSON of `{ shortcode → codepoint }` form.
- **The autocomplete dropdown's positioning** is tricky in textareas — modern browsers don't expose the cursor's pixel position directly. Either approximate via a hidden mirror div or use a textarea wrapper library. For v1, anchor the dropdown to the bottom-left of the textarea — good enough.
- **Existing FavoritesPanel uses CSS classes that may also be referenced elsewhere** — search for `favorites-` prefix CSS classes before deleting the component to ensure nothing else depends on them.

## End-of-roadmap intent (non-binding)

This document is Phase 2. Phase 2.5 will add: `use_count` tracking, most-used sort, drag-to-reorder, and possibly a standalone Unicode-emoji-insert picker (Discord-style smiley button) if `:name:` typing isn't enough in practice. The end-game vision (separate from any current spec) is a bundled Twemoji-style emoji set so all users see consistent emoji visuals regardless of OS — that swaps in via the single `renderUnicodeEmoji` function.
