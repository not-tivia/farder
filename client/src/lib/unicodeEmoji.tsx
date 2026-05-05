import { type ReactNode } from "react";

// Shortcode → emoji codepoint(s). Curated subset of the GitHub/Discord
// standard set covering the most commonly typed shortcodes. Add more as needed.
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

  // Misc
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
