# Reaction Book Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Inline `:name:` rendering in messages (book items + Unicode shorthand), `:name:` autocomplete dropdown above the textarea, and a slim `SendStickerPicker` that replaces the existing FavoritesPanel — opened from the same `*` button in the message input row.

**Architecture:** No protocol changes. Inline emoji ride on the existing message `attachments` array — when you type `:my-cat:`, the client uploads the cat image (cached per server) and includes its file_id in the message's attachments. On render, a token parser matches `:name:` tokens against book items by attachment filename and renders inline images at the token positions; consumed attachments are excluded from the standard attachment list below the message. Unicode shorthand (`:smile:` → 😄) flows through a single `renderUnicodeEmoji(codepoint)` function so swapping in a bundled Twemoji-style image set later is a one-file change.

**Tech Stack:** React + TypeScript, the existing Tauri commands and attachment infrastructure from Reaction Book Phase 1. One small Rust tweak to `book_get_file_for_server`.

**Spec:** `docs/superpowers/specs/2026-05-05-reaction-book-phase2-design.md`

**Predecessor:** Reaction Book Phase 1 (shipped 2026-05-05). Plan: `docs/superpowers/plans/2026-05-04-reaction-book-phase1.md`. Memory note: `~/.claude/projects/-home-deez-farder/memory/project_reaction_book.md`.

---

## File structure

**Modified Rust:**
- `client/src-tauri/src/book.rs` — `book_get_file_for_server` uploads with `original_name = "<book_name>.<ext>"` instead of `<id>.<ext>` so receiving clients can match the attachment to the inline-emoji token by filename.

**New TS:**
- `client/src/lib/unicodeEmoji.ts` — shortcode→codepoint map + `lookupShorthand(name)` + `renderUnicodeEmoji(codepoint)` (single point of change for cross-platform consistent rendering later)
- `client/src/components/InlineBookEmoji.tsx` — small `<img>` wrapper that uses the existing `imageCache` Map + `downloadFile` pattern from Message.tsx
- `client/src/components/RenderedMessageContent.tsx` — pure render component combining text, attachments, and book items into the inline-emoji-aware fragment
- `client/src/components/EmojiAutocomplete.tsx` — dropdown shown above the textarea while typing a `:name:` token
- `client/src/components/SendStickerPicker.tsx` — slim grid for click-to-send, replaces `FavoritesPanel`

**Modified TS:**
- `client/src/components/Message.tsx` — switch body render from raw text + AttachmentList to `<RenderedMessageContent>`
- `client/src/components/MessageInput.tsx` — repurpose `*` button to open `SendStickerPicker`; add autocomplete state + handler; pre-send tokenize-and-attach hook

**Deleted:**
- `client/src/components/FavoritesPanel.tsx`

---

## Task 1: Rust — `book_get_file_for_server` uploads with book name as filename

The existing implementation uploads `<item.id>.<ext>` (uuid). To support inline-emoji name matching on the receiving client, we need uploads to use `<item.name>.<ext>`. Existing cached file_ids (in `BookItem.server_files`) keep working — they were uploaded under the old name and the server has them under that name. Only NEW uploads (first-time use of an item on a server) get the new naming.

**Files:**
- Modify: `client/src-tauri/src/book.rs`

- [ ] **Step 1: Find the existing `book_get_file_for_server` function**

In `client/src-tauri/src/book.rs`, locate `pub async fn book_get_file_for_server`. Read it carefully to understand the existing flow (cache check → upload via `upload_file_internal` → store file_id in `server_files`).

- [ ] **Step 2: Change the source path that's uploaded**

Currently the function uploads the on-disk file at path `files_dir().join(format!("{}.{}", item.id, item.ext))`. The upload_file_internal extracts the filename from the source path. To get the server to record `original_name = "my-cat.png"` instead of `<uuid>.png`, we have two options:

**Option A (simpler — recommended):** Copy the file to a temp location with the desired filename, then upload from there. No change to upload_file_internal.

Replace the upload section of `book_get_file_for_server` with:

```rust
    let source_file_path = files_dir().join(format!("{}.{}", item.id, item.ext));
    if !source_file_path.exists() {
        return Err(format!("image file missing on disk for item {}", item.id));
    }

    // Upload with the book item's name as the filename so receiving clients
    // can match :name: tokens to the resulting attachment by original_name.
    // We achieve this by copying to a temp file named after the book item.
    let temp_dir = std::env::temp_dir();
    let safe_name = item.name.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_', "_");
    let temp_path = temp_dir.join(format!("farder-book-upload-{}-{}.{}", std::process::id(), safe_name, item.ext));
    std::fs::copy(&source_file_path, &temp_path).map_err(|e| format!("temp copy failed: {}", e))?;

    let path_str = temp_path.to_string_lossy().to_string();
    let upload_result = crate::commands::upload_file_internal(&state, &server_id, &path_str).await;
    let _ = std::fs::remove_file(&temp_path); // best-effort cleanup
    let file_id = upload_result.map_err(|e| format!("upload failed: {}", e))?;
```

Note: the temp file's filename includes the safe_name as a "rooted" segment so `upload_file_internal`'s name extraction picks up `<safe_name>.<ext>` (or whatever it derives from the basename). Inspect `upload_file_internal` in `commands.rs` to confirm how it pulls the filename from the path — adjust the temp filename pattern if necessary.

- [ ] **Step 3: Verify compile**

```
cd /home/deez/farder/client/src-tauri && cargo check 2>&1 | tail -3
```

Expected: `Finished` with the pre-existing dead-code warning only.

- [ ] **Step 4: Run book tests**

```
cd /home/deez/farder/client/src-tauri && cargo test --lib book::tests -- --test-threads=1 2>&1 | tail -10
```

Expected: all 3 existing tests still pass (they don't exercise upload, so no regression).

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src-tauri/src/book.rs
git -C /home/deez/farder commit -m "feat(client): book_get_file_for_server uploads with book name for inline-emoji matching"
```

---

## Task 2: `lib/unicodeEmoji.ts` — shortcode map + render function

Embed a small Map of shortcode → codepoint. The render function is the single point of change for future bundled-emoji-set work.

**Files:**
- Create: `client/src/lib/unicodeEmoji.ts`

- [ ] **Step 1: Create the file**

`client/src/lib/unicodeEmoji.ts`:

```ts
import { type ReactNode } from "react";

// Shortcode → emoji codepoint(s). Curated subset of the GitHub/Discord
// standard set covering the most commonly typed shortcodes. Add more as needed.
// Long-term: replace this with the full node-emoji data file (~1500 entries),
// or move to a server-side lookup if bundle size becomes a concern.
const SHORTCODES: Record<string, string> = {
  // Faces
  "smile": "😄", "laughing": "😆", "blush": "😊", "smiley": "😃",
  "smirk": "😏", "heart_eyes": "😍", "kissing_heart": "😘", "kissing": "😗",
  "wink": "😉", "stuck_out_tongue": "😛", "stuck_out_tongue_winking_eye": "😜",
  "sleeping": "😴", "sleepy": "😪", "expressionless": "😑", "neutral_face": "😐",
  "thinking": "🤔", "no_mouth": "😶", "rolling_eyes": "🙄", "smirk_cat": "😼",
  "scream": "😱", "fearful": "😨", "weary": "😩", "sob": "😭",
  "joy": "😂", "rofl": "🤣", "grin": "😁", "sweat_smile": "😅",
  "innocent": "😇", "yum": "😋", "sunglasses": "😎", "nerd_face": "🤓",
  "pleading": "🥺", "cry": "😢", "disappointed": "😞", "worried": "😟",
  "rage": "😡", "angry": "😠", "triumph": "😤", "skull": "💀",
  "ghost": "👻", "alien": "👽", "robot": "🤖", "poop": "💩",

  // Hearts
  "heart": "❤️", "yellow_heart": "💛", "green_heart": "💚",
  "blue_heart": "💙", "purple_heart": "💜", "black_heart": "🖤",
  "broken_heart": "💔", "two_hearts": "💕", "sparkling_heart": "💖",

  // Hands & gestures
  "thumbsup": "👍", "+1": "👍", "thumbsdown": "👎", "-1": "👎",
  "ok_hand": "👌", "wave": "👋", "clap": "👏", "raised_hands": "🙌",
  "pray": "🙏", "muscle": "💪", "point_up": "👆", "point_down": "👇",
  "point_left": "👈", "point_right": "👉", "v": "✌️", "metal": "🤘",

  // Symbols
  "fire": "🔥", "sparkles": "✨", "boom": "💥", "star": "⭐",
  "star2": "🌟", "zap": "⚡", "rainbow": "🌈", "100": "💯",
  "warning": "⚠️", "no_entry": "⛔", "white_check_mark": "✅",
  "x": "❌", "heavy_check_mark": "✔️", "heavy_multiplication_x": "✖️",
  "tada": "🎉", "confetti_ball": "🎊", "balloon": "🎈", "gift": "🎁",
  "trophy": "🏆", "medal": "🏅", "rocket": "🚀", "crown": "👑",

  // Animals
  "dog": "🐶", "cat": "🐱", "mouse": "🐭", "hamster": "🐹",
  "rabbit": "🐰", "fox": "🦊", "bear": "🐻", "panda_face": "🐼",
  "koala": "🐨", "tiger": "🐯", "lion": "🦁", "cow": "🐮",
  "pig": "🐷", "frog": "🐸", "monkey_face": "🐵", "chicken": "🐔",
  "penguin": "🐧", "bird": "🐦", "owl": "🦉", "wolf": "🐺",
  "boar": "🐗", "horse": "🐴", "unicorn": "🦄", "bee": "🐝",
  "snail": "🐌", "butterfly": "🦋", "snake": "🐍", "turtle": "🐢",
  "fish": "🐟", "whale": "🐳", "dolphin": "🐬", "octopus": "🐙",

  // Food
  "pizza": "🍕", "hamburger": "🍔", "fries": "🍟", "hotdog": "🌭",
  "taco": "🌮", "burrito": "🌯", "popcorn": "🍿", "doughnut": "🍩",
  "cookie": "🍪", "birthday": "🎂", "cake": "🍰", "chocolate_bar": "🍫",
  "candy": "🍬", "lollipop": "🍭", "icecream": "🍦", "coffee": "☕",
  "tea": "🍵", "beer": "🍺", "beers": "🍻", "wine_glass": "🍷",
  "cocktail": "🍸", "tropical_drink": "🍹", "champagne": "🍾",
  "apple": "🍎", "banana": "🍌", "watermelon": "🍉", "grapes": "🍇",
  "strawberry": "🍓", "peach": "🍑", "cherries": "🍒", "tangerine": "🍊",

  // Misc that comes up often
  "eyes": "👀", "ear": "👂", "tongue": "👅", "lips": "👄",
  "computer": "💻", "phone": "📱", "headphones": "🎧", "musical_note": "🎵",
  "books": "📚", "book": "📖", "pencil": "✏️", "memo": "📝",
  "clock": "🕐", "alarm_clock": "⏰", "hourglass": "⌛", "moon": "🌙",
  "sun": "☀️", "cloud": "☁️", "snowflake": "❄️", "umbrella": "☂️",
  "earth_americas": "🌎", "earth_africa": "🌍", "earth_asia": "🌏",
};

/** Lookup a shortcode name (without the colons). Returns the emoji codepoint
 *  string, or null if not found. */
export function lookupShorthand(name: string): string | null {
  return SHORTCODES[name.toLowerCase()] ?? null;
}

/** Get all shortcodes matching a query (for autocomplete). Returns up to `limit`
 *  names sorted alphabetically. */
export function searchShorthand(query: string, limit: number): string[] {
  const q = query.toLowerCase();
  const matches: string[] = [];
  for (const name of Object.keys(SHORTCODES)) {
    if (name.startsWith(q)) {
      matches.push(name);
      if (matches.length >= limit) break;
    }
  }
  return matches.sort();
}

/**
 * Render a Unicode emoji codepoint as a React node.
 *
 * SINGLE POINT OF CHANGE for end-game cross-platform-consistent rendering.
 * Today: returns the codepoint as a span (OS renders via system emoji font).
 * Future: swap to <img src={`/emoji/${hex(codepoint)}.svg`} /> for bundled
 * Twemoji or equivalent — every other call site stays the same.
 */
export function renderUnicodeEmoji(codepoint: string): ReactNode {
  return <span className="unicode-emoji">{codepoint}</span>;
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/lib/unicodeEmoji.ts
git -C /home/deez/farder commit -m "feat(client): unicode emoji shortcode map + render function (single swap point)"
```

---

## Task 3: `InlineBookEmoji` component

A small `<img>` wrapper that uses the same imageCache + downloadFile pattern as the existing `AttachmentDisplay` and `ReactionBadge` from Phase 1.

**Files:**
- Create: `client/src/components/InlineBookEmoji.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/InlineBookEmoji.tsx`:

```tsx
import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import type { AttachmentInfo } from "../lib/types";

// Module-level cache — same pattern as Message.tsx::imageCache. Sharing across
// instances is the goal so a single attachment isn't downloaded multiple times.
const inlineImageCache = new Map<number, string>();

interface Props {
  attachment: AttachmentInfo;
  serverId: string;
  altText: string;
}

export default function InlineBookEmoji({ attachment, serverId, altText }: Props) {
  const [imageUrl, setImageUrl] = useState<string | null>(
    inlineImageCache.get(attachment.file_id) ?? null,
  );

  useEffect(() => {
    if (imageUrl != null) return;
    let cancelled = false;
    api.downloadFile(serverId, attachment.file_id).then((r) => {
      if (!cancelled && r.data_url) {
        inlineImageCache.set(attachment.file_id, r.data_url);
        setImageUrl(r.data_url);
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [attachment.file_id, serverId, imageUrl]);

  if (imageUrl == null) {
    return <span style={{ fontSize: 10, opacity: 0.5 }}>…</span>;
  }

  return (
    <img
      src={imageUrl}
      alt={altText}
      title={altText}
      className="inline-book-emoji"
      style={{
        width: "1.4em",
        height: "1.4em",
        objectFit: "contain",
        verticalAlign: "middle",
        margin: "0 1px",
      }}
    />
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/InlineBookEmoji.tsx
git -C /home/deez/farder commit -m "feat(client): InlineBookEmoji wrapper with shared image cache"
```

---

## Task 4: `RenderedMessageContent` — pure text+attachment renderer

Token parser + matcher + render. No state — purely a function of its props.

**Files:**
- Create: `client/src/components/RenderedMessageContent.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/RenderedMessageContent.tsx`:

```tsx
import { type ReactNode } from "react";
import type { AttachmentInfo } from "../lib/types";
import type { BookItem } from "../lib/book/types";
import { lookupShorthand, renderUnicodeEmoji } from "../lib/unicodeEmoji";
import InlineBookEmoji from "./InlineBookEmoji";

interface Token {
  name: string;     // text inside the colons, lowercased
  index: number;    // start position in source text (the opening colon)
  length: number;   // total length including both colons
}

const TOKEN_REGEX = /:([a-z0-9_+\-]+):/gi;

export function parseColonTokens(text: string): Token[] {
  const tokens: Token[] = [];
  let m: RegExpExecArray | null;
  TOKEN_REGEX.lastIndex = 0;
  while ((m = TOKEN_REGEX.exec(text)) !== null) {
    tokens.push({
      name: m[1].toLowerCase(),
      index: m.index,
      length: m[0].length,
    });
  }
  return tokens;
}

interface Props {
  text: string;
  attachments: AttachmentInfo[];
  bookIndex: BookItem[];
  serverId: string;
  /** Render attachments not consumed as inline emoji. Pass through unchanged. */
  renderRemainingAttachments: (remaining: AttachmentInfo[]) => ReactNode;
}

export default function RenderedMessageContent({
  text,
  attachments,
  bookIndex,
  serverId,
  renderRemainingAttachments,
}: Props) {
  const tokens = parseColonTokens(text);
  const usedAttachmentIds = new Set<number>();
  const out: ReactNode[] = [];
  let cursor = 0;
  let key = 0;

  for (const tok of tokens) {
    // Emit the text segment before this token (preserving line breaks etc).
    if (tok.index > cursor) {
      out.push(<span key={key++}>{text.slice(cursor, tok.index)}</span>);
    }
    cursor = tok.index + tok.length;

    // Try book item match first (book wins on collision).
    const bookMatch = bookIndex.find((b) => b.name === tok.name);
    if (bookMatch) {
      const expectedFilename = `${bookMatch.name}.${bookMatch.ext}`;
      const att = attachments.find((a) =>
        a.original_name === expectedFilename && !usedAttachmentIds.has(a.file_id),
      );
      if (att) {
        usedAttachmentIds.add(att.file_id);
        out.push(
          <InlineBookEmoji
            key={key++}
            attachment={att}
            serverId={serverId}
            altText={`:${tok.name}:`}
          />,
        );
        continue;
      }
    }

    // Try Unicode shorthand.
    const codepoint = lookupShorthand(tok.name);
    if (codepoint) {
      out.push(<span key={key++}>{renderUnicodeEmoji(codepoint)}</span>);
      continue;
    }

    // No match — render the token as literal text.
    out.push(<span key={key++}>{text.slice(tok.index, tok.index + tok.length)}</span>);
  }

  // Trailing text after the last token.
  if (cursor < text.length) {
    out.push(<span key={key++}>{text.slice(cursor)}</span>);
  }

  // Attachments not consumed as inline emoji render normally below the text.
  const remaining = attachments.filter((a) => !usedAttachmentIds.has(a.file_id));

  return (
    <>
      <span className="message-content">{out}</span>
      {renderRemainingAttachments(remaining)}
    </>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. If `AttachmentInfo` doesn't have `original_name` / `file_id` fields with those exact names, open `lib/types.ts` and adjust the references to match.

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/RenderedMessageContent.tsx
git -C /home/deez/farder commit -m "feat(client): RenderedMessageContent — token-aware message render"
```

---

## Task 5: `EmojiAutocomplete` component

Small dropdown that floats above the message input while typing a `:name:` token.

**Files:**
- Create: `client/src/components/EmojiAutocomplete.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/EmojiAutocomplete.tsx`:

```tsx
import { useEffect, useState, type CSSProperties } from "react";
import type { BookItem } from "../lib/book/types";
import { searchShorthand, lookupShorthand, renderUnicodeEmoji } from "../lib/unicodeEmoji";
import { useBookItemSrc } from "./BookItemTile";

interface Match {
  kind: "book" | "unicode";
  name: string;
  // For book items, the BookItem (used for the inline thumbnail).
  bookItem?: BookItem;
}

interface Props {
  query: string;
  bookIndex: BookItem[];
  position: { x: number; y: number };
  onSelect: (name: string) => void;
  onClose: () => void;
}

const popover: CSSProperties = {
  position: "fixed",
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 4,
  minWidth: 160,
  zIndex: 1500,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
};

const itemStyle = (selected: boolean): CSSProperties => ({
  display: "flex",
  alignItems: "center",
  gap: 8,
  padding: "4px 8px",
  background: selected ? "var(--xp-blue, #0058E6)" : "transparent",
  color: selected ? "#fff" : "inherit",
  cursor: "pointer",
});

const MAX_RESULTS = 8;

function BookThumb({ item }: { item: BookItem }) {
  const src = useBookItemSrc(item.id);
  return <img src={src} alt={item.name} style={{ width: 18, height: 18, objectFit: "contain" }} />;
}

export default function EmojiAutocomplete({ query, bookIndex, position, onSelect, onClose }: Props) {
  const [selectedIdx, setSelectedIdx] = useState(0);

  // Build the result list: book items first (alphabetical), then Unicode shorthand.
  const q = query.toLowerCase();
  const bookMatches: Match[] = bookIndex
    .filter((b) => b.name.toLowerCase().startsWith(q))
    .slice(0, MAX_RESULTS)
    .map((b) => ({ kind: "book", name: b.name, bookItem: b }));
  const remaining = MAX_RESULTS - bookMatches.length;
  const unicodeMatches: Match[] =
    remaining > 0
      ? searchShorthand(q, remaining).map((name) => ({ kind: "unicode", name }))
      : [];
  const matches = [...bookMatches, ...unicodeMatches];

  // Reset selection when the result list changes (e.g. user typed another char).
  useEffect(() => {
    setSelectedIdx(0);
  }, [query, matches.length]);

  // Keyboard navigation handled by the parent textarea; we just expose handlers.
  // The parent calls handleKeyDown via a ref or via a global keydown listener
  // we install here.
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (matches.length === 0) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIdx((i) => (i + 1) % matches.length);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIdx((i) => (i - 1 + matches.length) % matches.length);
      } else if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        onSelect(matches[selectedIdx].name);
      } else if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    }
    window.addEventListener("keydown", handleKey, true); // capture phase
    return () => window.removeEventListener("keydown", handleKey, true);
  }, [matches, selectedIdx, onSelect, onClose]);

  if (matches.length === 0) return null;

  return (
    <div style={{ ...popover, top: position.y, left: position.x }}>
      {matches.map((m, i) => (
        <div
          key={`${m.kind}-${m.name}`}
          style={itemStyle(i === selectedIdx)}
          onMouseEnter={() => setSelectedIdx(i)}
          onClick={() => onSelect(m.name)}
        >
          {m.kind === "book" && m.bookItem ? (
            <BookThumb item={m.bookItem} />
          ) : (
            <span style={{ width: 18, textAlign: "center" }}>
              {renderUnicodeEmoji(lookupShorthand(m.name) ?? "")}
            </span>
          )}
          <span>:{m.name}:</span>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. If `useBookItemSrc` isn't exported from BookItemTile, add `export` to it (it was created in Phase 1 Task 14 but may not be exported).

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/EmojiAutocomplete.tsx client/src/components/BookItemTile.tsx
git -C /home/deez/farder commit -m "feat(client): EmojiAutocomplete dropdown with arrow nav + book/unicode mix"
```

(Stage `BookItemTile.tsx` only if you needed to add `export` to `useBookItemSrc`.)

---

## Task 6: `SendStickerPicker` component

Slim grid that replaces FavoritesPanel — click to send.

**Files:**
- Create: `client/src/components/SendStickerPicker.tsx`

- [ ] **Step 1: Create the file**

`client/src/components/SendStickerPicker.tsx`:

```tsx
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
import { useBookItemSrc } from "./BookItemTile";

interface Props {
  serverId: string;
  channelId: number;
  onClose: () => void;
}

const popover: CSSProperties = {
  position: "absolute",
  bottom: "calc(100% + 4px)",
  left: 0,
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 8,
  width: 320,
  maxHeight: 380,
  zIndex: 1100,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
  display: "flex",
  flexDirection: "column",
  gap: 6,
};

function StickerTile({
  item,
  onSend,
}: {
  item: BookItem;
  onSend: (item: BookItem) => void;
}) {
  const src = useBookItemSrc(item.id);
  return (
    <button
      onClick={() => onSend(item)}
      title={`:${item.name}:`}
      style={{
        width: 64,
        height: 64,
        padding: 0,
        background: "transparent",
        border: "1px solid transparent",
        cursor: "pointer",
      }}
    >
      <img
        src={src}
        alt={item.name}
        style={{ width: "100%", height: "100%", objectFit: "contain" }}
      />
    </button>
  );
}

export default function SendStickerPicker({ serverId, channelId, onClose }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [items, setItems] = useState<BookItem[]>([]);
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    bookApi.bookListItems().then(setItems).catch((e) => setError(String(e)));
  }, []);

  // Close on outside click + Esc.
  useEffect(() => {
    function handleMouse(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  const visible = useMemo(() => {
    let out = [...items].sort((a, b) => b.added_at - a.added_at);
    if (search.trim()) {
      const q = search.toLowerCase();
      out = out.filter((i) => i.name.toLowerCase().includes(q));
    }
    return out;
  }, [items, search]);

  async function send(item: BookItem) {
    try {
      const fileId = await bookApi.bookGetFileForServer(serverId, item.id);
      await api.sendMessage(serverId, channelId, "", undefined, [fileId]);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div ref={ref} style={popover}>
      <input
        autoFocus
        placeholder="Search…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ font: "inherit", padding: "2px 6px" }}
      />
      {error && <div style={{ color: "#a00", fontSize: 10 }}>{error}</div>}
      <div style={{ overflowY: "auto", display: "flex", flexWrap: "wrap", gap: 4 }}>
        {visible.length === 0 && !error && (
          <div style={{ padding: 16, color: "var(--xp-text-muted, #666)", textAlign: "center", width: "100%" }}>
            {items.length === 0
              ? "Your book is empty. Open the 📚 button to add items."
              : "No items match your search."}
          </div>
        )}
        {visible.map((item) => (
          <StickerTile key={item.id} item={item} onSend={send} />
        ))}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add client/src/components/SendStickerPicker.tsx
git -C /home/deez/farder commit -m "feat(client): SendStickerPicker — click-to-send book items as standalone messages"
```

---

## Task 7: Wire `RenderedMessageContent` into `Message.tsx`

**Files:**
- Modify: `client/src/components/Message.tsx`

- [ ] **Step 1: Add imports + book index loader**

In `client/src/components/Message.tsx`, near the top imports add:

```tsx
import RenderedMessageContent from "./RenderedMessageContent";
```

(Existing `bookApi` import from Phase 1 should already be there.)

Add a module-level cached book index — same pattern as `cachedOwnPk`:

```tsx
let cachedBookIndex: BookItem[] = [];
let bookIndexLoadPromise: Promise<void> | null = null;
function loadBookIndex(): Promise<void> {
  if (!bookIndexLoadPromise) {
    bookIndexLoadPromise = bookApi.bookListItems()
      .then((items) => { cachedBookIndex = items; })
      .catch(() => {});
  }
  return bookIndexLoadPromise;
}
```

(Place this near the existing `cachedOwnPk` declaration around line 58.)

- [ ] **Step 2: Refresh book index on every message render**

Inside the `Message` component, add:

```tsx
const [bookIndex, setBookIndex] = useState<BookItem[]>(cachedBookIndex);
useEffect(() => {
  let cancelled = false;
  loadBookIndex().then(() => {
    if (!cancelled) setBookIndex(cachedBookIndex);
  });
  return () => { cancelled = true; };
}, []);
```

(This way every Message gets the latest cached index after the initial async load. Subsequent additions to the book will be reflected after the user reopens BookBrowser — for now, no need to live-refresh on every book mutation.)

- [ ] **Step 3: Replace the message-body render with RenderedMessageContent**

Find where the existing message body + attachments are rendered. The current pattern is roughly:

```tsx
<div className="message-body">
  {renderContent(displayContent, memberNames, ownDisplayName)}
</div>
{message.attachments.length > 0 && (
  <div className="message-attachments">
    {message.attachments.map((att) => (
      <AttachmentDisplay key={att.id} attachment={att} messageContent={message.content} serverId={serverId} />
    ))}
  </div>
)}
```

Replace with:

```tsx
<div className="message-body">
  <RenderedMessageContent
    text={displayContent}
    attachments={message.attachments}
    bookIndex={bookIndex}
    serverId={serverId}
    renderRemainingAttachments={(remaining) =>
      remaining.length > 0 ? (
        <div className="message-attachments">
          {remaining.map((att) => (
            <AttachmentDisplay key={att.id} attachment={att} messageContent={message.content} serverId={serverId} />
          ))}
        </div>
      ) : null
    }
  />
</div>
```

NOTE: the `renderContent` function (which handles @mentions) is bypassed here. RenderedMessageContent only handles text + token expansion. To preserve mention rendering, RenderedMessageContent's text spans need to ALSO go through `renderContent`. For a quick v1, leave mentions broken inside emoji-rendered messages and add a follow-up TODO. OR — better — pass a `renderTextSegment` prop into RenderedMessageContent that the parent provides (calling `renderContent` for each plain-text segment).

Add the segment renderer prop. In `RenderedMessageContent.tsx`, add an optional prop `renderTextSegment?: (text: string) => ReactNode` defaulting to `(t) => <>{t}</>`. Use it for every text-segment span. Then in Message.tsx, pass `renderTextSegment={(t) => renderContent(t, memberNames, ownDisplayName)}`.

- [ ] **Step 4: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`.

- [ ] **Step 5: Commit**

```
git -C /home/deez/farder add client/src/components/Message.tsx client/src/components/RenderedMessageContent.tsx
git -C /home/deez/farder commit -m "feat(client): Message body uses RenderedMessageContent + preserves mention rendering"
```

---

## Task 8: Wire MessageInput — repurpose `*` button + autocomplete + pre-send tokenize

**Files:**
- Modify: `client/src/components/MessageInput.tsx`
- Delete: `client/src/components/FavoritesPanel.tsx`

- [ ] **Step 1: Replace FavoritesPanel import + state**

In `client/src/components/MessageInput.tsx`, find:

```tsx
import FavoritesPanel from "./FavoritesPanel";
```

Replace with:

```tsx
import SendStickerPicker from "./SendStickerPicker";
import EmojiAutocomplete from "./EmojiAutocomplete";
import * as bookApi from "../lib/book/client";
import type { BookItem } from "../lib/book/types";
```

Find:

```tsx
const [showFavorites, setShowFavorites] = useState(false);
```

Replace with:

```tsx
const [showStickerPicker, setShowStickerPicker] = useState(false);
const [bookIndex, setBookIndex] = useState<BookItem[]>([]);
const [autocompleteQuery, setAutocompleteQuery] = useState<string | null>(null);
const [autocompletePos, setAutocompletePos] = useState<{ x: number; y: number } | null>(null);
const textareaWrapperRef = useRef<HTMLDivElement | null>(null);
```

(`useRef` should already be imported — confirm.)

Load the book index on mount:

```tsx
useEffect(() => {
  bookApi.bookListItems().then(setBookIndex).catch(() => {});
}, []);
```

- [ ] **Step 2: Add token-position detector**

Below the existing handlers, add:

```tsx
function detectTokenAtCursor(content: string, cursor: number): string | null {
  // Walk backward from cursor to find a ":" with at least 2 word chars after it
  // and no closing ":" between.
  let i = cursor - 1;
  while (i >= 0) {
    const c = content[i];
    if (c === ":") {
      const after = content.slice(i + 1, cursor);
      // Reject if contains another colon, whitespace, or is too short.
      if (after.length < 2 || /[:\s]/.test(after)) return null;
      // Confirm it's a valid token char set.
      if (!/^[a-z0-9_+\-]+$/i.test(after)) return null;
      return after;
    }
    if (/[\s:]/.test(c)) return null;
    i--;
  }
  return null;
}
```

- [ ] **Step 3: Update `handleChange` to detect tokens**

Find the existing `handleChange` and add the token detection at the end:

```tsx
function handleChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
  const next = e.target.value;
  setContent(next);
  // ... existing typing-indicator + mention handling ...

  // Token autocomplete.
  const cursor = e.target.selectionStart;
  const query = detectTokenAtCursor(next, cursor);
  setAutocompleteQuery(query);
  if (query) {
    const rect = textareaWrapperRef.current?.getBoundingClientRect();
    if (rect) {
      // Anchor the autocomplete above the textarea, left-aligned.
      setAutocompletePos({ x: rect.left, y: rect.top - 4 });
    }
  } else {
    setAutocompletePos(null);
  }
}
```

(The existing handleChange has typing-indicator + @mention logic — preserve those unchanged. Add the token detection block at the end.)

- [ ] **Step 4: Add a token-insert handler for autocomplete selection**

```tsx
function handleAutocompleteSelect(name: string) {
  const ta = textareaRef.current;
  if (!ta) return;
  const cursor = ta.selectionStart;
  // Find the colon position — walk backward.
  let colonIdx = cursor - 1;
  while (colonIdx >= 0 && content[colonIdx] !== ":") colonIdx--;
  if (colonIdx < 0) return;

  const before = content.slice(0, colonIdx);
  const after = content.slice(cursor);
  const inserted = `:${name}: `;
  const next = before + inserted + after;
  setContent(next);
  setAutocompleteQuery(null);
  setAutocompletePos(null);
  // Restore focus + cursor.
  setTimeout(() => {
    ta.focus();
    const newCursor = colonIdx + inserted.length;
    ta.setSelectionRange(newCursor, newCursor);
  }, 0);
}
```

- [ ] **Step 5: Hook the inline-emoji upload into the send flow**

Find the existing `handleSend` function. Before it dispatches `api.sendMessage`, add a step that resolves any `:name:` tokens in the text to their book file_ids and adds them to the attachments array.

```tsx
async function resolveInlineEmojiAttachments(text: string, currentAttachments: number[]): Promise<number[]> {
  // Re-parse tokens (don't import RenderedMessageContent's parseColonTokens here
  // to avoid React deps; inline a small regex match).
  const tokens: string[] = [];
  const re = /:([a-z0-9_+\-]+):/gi;
  let m;
  while ((m = re.exec(text)) !== null) {
    tokens.push(m[1].toLowerCase());
  }

  const out = [...currentAttachments];
  const seen = new Set<string>();
  for (const name of tokens) {
    if (seen.has(name)) continue;
    seen.add(name);
    const item = bookIndex.find((b) => b.name === name);
    if (!item) continue;
    try {
      const fileId = await bookApi.bookGetFileForServer(serverId, item.id);
      if (!out.includes(fileId)) out.push(fileId);
    } catch (e) {
      console.error("[message-input:inline-emoji]", name, e);
    }
  }
  return out;
}
```

In `handleSend`, after computing the `attachments` array but before calling `api.sendMessage`, do:

```tsx
const finalAttachments = await resolveInlineEmojiAttachments(text, attachments);
// ... then call api.sendMessage(... finalAttachments ...);
```

(Adjust to match the exact existing structure — read the existing handleSend body carefully.)

- [ ] **Step 6: Replace the `*` button onClick + render**

Find the `*` button in the JSX:

```tsx
<button
  className="xp-button attach-btn"
  onClick={() => setShowFavorites(!showFavorites)}
  disabled={sending}
  title="Favorites"
>
  *
</button>
```

Replace with:

```tsx
<button
  className="xp-button attach-btn"
  onClick={() => setShowStickerPicker((s) => !s)}
  disabled={sending}
  title="Send Sticker"
>
  🎁
</button>
```

(The icon can be `*` or `🎁` or whatever you prefer — `🎁` matches Discord's sticker button icon.)

Wrap the textarea (and possibly the button row) in a div that holds the `textareaWrapperRef`:

```tsx
<div ref={textareaWrapperRef} style={{ position: "relative", flex: 1 }}>
  <textarea
    ref={textareaRef}
    /* ... existing props ... */
  />
  {showStickerPicker && currentChannelId !== null && (
    <SendStickerPicker
      serverId={serverId}
      channelId={channelId}
      onClose={() => setShowStickerPicker(false)}
    />
  )}
</div>
```

- [ ] **Step 7: Render the autocomplete dropdown**

Just before the closing tag of the message input area:

```tsx
{autocompleteQuery !== null && autocompletePos && (
  <EmojiAutocomplete
    query={autocompleteQuery}
    bookIndex={bookIndex}
    position={autocompletePos}
    onSelect={handleAutocompleteSelect}
    onClose={() => { setAutocompleteQuery(null); setAutocompletePos(null); }}
  />
)}
```

- [ ] **Step 8: Find and remove all FavoritesPanel render references**

Search for `FavoritesPanel` in the file:

```
grep -n "FavoritesPanel\|showFavorites" client/src/components/MessageInput.tsx
```

Delete any conditional render of FavoritesPanel that remains.

- [ ] **Step 9: Delete the FavoritesPanel component file**

```
git -C /home/deez/farder rm client/src/components/FavoritesPanel.tsx
```

(Search the codebase for any other references first — `grep -rn "FavoritesPanel" client/src/`. If any remain, delete those references or fix the imports.)

- [ ] **Step 10: Verify TS compiles**

```
cd /home/deez/farder/client && npx tsc --noEmit 2>&1; echo "(exit $?)"
```

Expected: `(exit 0)`. If you see "Cannot find name 'FavoritesPanel'" anywhere, that's a leftover reference — fix it.

- [ ] **Step 11: Commit**

```
git -C /home/deez/farder add client/src/components/MessageInput.tsx
git -C /home/deez/farder commit -m "feat(client): MessageInput repurposed * → SendStickerPicker, autocomplete + inline-emoji send hook"
```

---

## Task 9: End-to-end smoke + CHANGELOG

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Restart dev session and walk smoke tests**

```
pkill -f farder-server
cd /home/deez/farder/client && npm run tauri dev
```

Confirm:

- [ ] Type `:smile:` in a message → renders as 😄 inline after send.
- [ ] Type `:my-cat:` (where my-cat is a book item) → renders as the cat image inline. Both you and other clients see it.
- [ ] Type `:nonsense_word:` → stays as literal text.
- [ ] Type `:my` (cursor at end) → autocomplete dropdown appears. Book matches first, Unicode after.
- [ ] Arrow keys + Enter → token gets inserted (`:my-cat: ` with trailing space).
- [ ] Esc → autocomplete closes.
- [ ] Click `🎁` button (formerly `*`) → SendStickerPicker opens. Click a thumbnail → message sends as a sticker. Picker closes.
- [ ] FavoritesPanel does NOT exist anywhere in the UI.
- [ ] Edit a sent message → source `:my-cat:` text appears in the textarea (not a rendered image).
- [ ] Search for `cat` → matches messages containing `:my-cat:`.
- [ ] Sender sees their own messages rendered the same way receivers do.
- [ ] Old client (if you can simulate one) on the same server: receives messages with `:name:` text + attachment; renders text literally + attachment as image. Functional but not inline.

- [ ] **Step 2: Add CHANGELOG entry**

In `CHANGELOG.md`, under the most recent `### Added` block, add:

```
- (2026-05-05) Reaction Book Phase 2: inline `:name:` rendering in messages (typed `:my-cat:` becomes the book image inline; `:smile:` → 😄 via Unicode shorthand). Autocomplete dropdown above the textarea while typing — book items first, Unicode after. Arrow keys + Enter to select. The existing `*` Favorites button is repurposed to a 🎁 SendStickerPicker — click to send a book item as a standalone sticker message. FavoritesPanel removed (the book absorbed the storage in Phase 1; this removes the now-vestigial UI). No protocol changes — inline emoji ride on the existing message attachments array via filename matching. Unicode rendering goes through a single `renderUnicodeEmoji()` function so swapping in a bundled cross-platform emoji set later is a one-file change.
```

- [ ] **Step 3: Commit**

```
git -C /home/deez/farder add CHANGELOG.md
git -C /home/deez/farder commit -m "docs: changelog for reaction book phase 2"
```

---

## Self-review notes

**Spec coverage:**
- Inline `:name:` rendering → Tasks 4, 7
- Unicode shorthand → Tasks 2, 4
- Autocomplete dropdown → Tasks 5, 8
- Send-as-sticker picker → Tasks 6, 8
- Attachment filename matching → Tasks 1, 4
- `renderUnicodeEmoji` single point of change → Task 2
- FavoritesPanel removal → Task 8
- Edit/search/sender-render preservation → Task 7 (RenderedMessageContent is purely display, source preserved)
- Backwards compat (no protocol changes) → spec says no protocol changes; verified across all tasks

**Type/name consistency:** `BookItem` shape consistent across modules (Tasks 4, 5, 6, 8). `Token` shape only used inside `RenderedMessageContent.tsx`. `parseColonTokens` is module-internal but exported for the autocomplete path's parse-once-from-textarea use case (could be exported if needed).

**No placeholders:** every code step has runnable code. The "read existing handleSend body" note in Task 8 is necessary because that function has channel-specific upload logic the implementer must preserve while adding the inline-emoji resolution.

**Known compromise:**
- Multi-use of same token in one message renders only the first as inline (Task 4 noted). Acceptable v1 — rare case.
- The autocomplete position approximation (anchoring to textarea bounds rather than cursor pixels) is good-enough; pixel-precise textarea cursor tracking is a known browser-API gap that's not worth implementing for v1.
- The book index in Message.tsx + MessageInput.tsx is loaded once; if the user adds new book items, they won't be available for inline rendering until the next message render mounts. Reasonable; users expect a brief refresh delay.
