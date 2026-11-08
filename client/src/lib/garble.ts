// The speech garbler's brain: how a typed word is replaced. Pure -- no DOM, no
// storage, no network -- so it can be tested on its own (tests/garble.test.mjs).
//
// Ported from the screen censor app's garble.py. Everything here transforms text
// the SENDER chose to have transformed, on the sender's own device, before it is
// sent. The garbled words are what everyone (including the sender) then sees.

export const SOUNDS = {
  bark: ["bark", "woof", "arf", "ruff", "yip", "grr"],
  meow: ["meow", "mew", "mrrp", "purr", "nya"],
  moo: ["moo", "mooo", "moooo"],
  mumble: ["mmph", "mmm", "hmm", "mrph"],
} as const;

export const MODES = ["bark", "meow", "moo", "mumble", "scramble", "shuffle", "custom"] as const;
export type GarbleMode = (typeof MODES)[number];

export const MODE_LABELS: Record<GarbleMode, string> = {
  bark: "Bark",
  meow: "Meow",
  moo: "Moo",
  mumble: "Mumble",
  scramble: "Scramble (random letters)",
  shuffle: "Shuffle (same letters, jumbled)",
  custom: "Custom phrase",
};

export const MAX_PHRASE = 40;

export type Rng = () => number; // [0, 1)
export type Transform = (word: string) => string;

/** A short, printable, single-line phrase, or "" if nothing usable is left. */
export function sanitizePhrase(text: string): string {
  let out = "";
  for (const ch of String(text)) {
    if (/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/u.test(ch)) continue; // controls, newlines, invisibles
    out += ch;
  }
  return Array.from(out).slice(0, MAX_PHRASE).join("").trim();
}

function pick<T>(items: readonly T[], rng: Rng): T {
  return items[Math.min(items.length - 1, Math.floor(rng() * items.length))];
}

function isAllUpper(s: string): boolean {
  return s === s.toUpperCase() && s !== s.toLowerCase();
}

/** Give `out` the capitalisation shape of `src` (WORD -> WOOF, Word -> Woof). */
export function matchCase(src: string, out: string): string {
  const chars = Array.from(src);
  if (chars.length > 1 && isAllUpper(src)) return out.toUpperCase();
  const first = chars[0] ?? "";
  if (first && first === first.toUpperCase() && first !== first.toLowerCase()) {
    return out.charAt(0).toUpperCase() + out.slice(1);
  }
  return out;
}

export function makeTransform(mode: GarbleMode, phrase = "", rng: Rng = Math.random): Transform {
  if (mode === "bark" || mode === "meow" || mode === "moo" || mode === "mumble") {
    const choices = SOUNDS[mode];
    return (w) => matchCase(w, pick(choices, rng));
  }
  if (mode === "scramble") {
    const letters = "abcdefghijklmnopqrstuvwxyz";
    return (w) =>
      matchCase(w, Array.from(w, () => letters[Math.min(25, Math.floor(rng() * 26))]).join(""));
  }
  if (mode === "shuffle") {
    return (w) => {
      const chars = Array.from(w);
      for (let attempt = 0; attempt < 5; attempt++) {
        for (let i = chars.length - 1; i > 0; i--) {
          const j = Math.floor(rng() * (i + 1));
          [chars[i], chars[j]] = [chars[j], chars[i]];
        }
        if (chars.join("") !== w || new Set(chars).size === 1) break;
      }
      return chars.join("");
    };
  }
  if (mode === "custom") {
    const text = sanitizePhrase(phrase) || "...";
    return () => text;
  }
  throw new Error(`unknown garble mode ${String(mode)}`);
}

// Words and contractions; underscores and symbols are left alone.
const WORD_RE = /[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*/gu;

// Things that must survive untouched or the message stops working: code, links,
// <...> tokens, @mentions, and :emoji: book tokens. The capture group makes
// split() return them at the odd indices.
const PROTECTED_RE =
  /(```[\s\S]*?```|`[^`\n]*`|https?:\/\/\S+|<[^>\s]+>|@\S+|:[A-Za-z0-9_+-]+:)/g;

/** Replace every word in `text`; protected tokens, spacing and punctuation stay as they are. */
export function garbleText(text: string, transform: Transform): string {
  return text
    .split(PROTECTED_RE)
    .map((part, i) => (i % 2 === 1 ? part : part.replace(WORD_RE, (w) => transform(w))))
    .join("");
}
