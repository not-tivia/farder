import { useMemo } from "react";

/**
 * The animated flourish behind someone's profile card.
 *
 * **Every effect is drawn by this file from shapes and keyframes — there are no
 * image assets, and nothing is ever fetched to render one.** That is the whole
 * design, and it is not an optimisation:
 *
 *  - A profile carries an ID (`"bats"`), not a URL or a blob. Opening someone's
 *    profile therefore makes no request to anywhere, so a profile cannot be used
 *    to learn who looked at it, or to make a viewer's client download a
 *    stranger's file.
 *  - Nothing is uploaded, so an effect costs the server no storage and creates
 *    no moderation surface. There is no way to put something vile in one.
 *  - It works offline and in an encrypted channel, because there is nothing to
 *    resolve.
 *
 * The id namespace is deliberately prefix-free today (`bats`, `snow`, …). If
 * community-made effects ever ship from the website, they should arrive as an
 * INSTALLED pack — fetched once, when the person chooses to install it, exactly
 * like a theme — and take a namespaced id (`pack:halloween/bats`). Fetching at
 * view time would undo every property above.
 */

export interface ProfileEffectDef {
  id: string;
  name: string;
  /** What it looks like, for the picker. */
  blurb: string;
  /** The glyph each particle draws. */
  glyph: string;
  count: number;
  /** Seconds for one traversal; each particle varies around it. */
  duration: number;
  /** Extra CSS applied to every particle of this effect. */
  particleStyle?: React.CSSProperties;
  /** Which keyframe animation to use. */
  motion: "drift-across" | "fall" | "rise" | "twinkle";
}

export const PROFILE_EFFECTS: ProfileEffectDef[] = [
  {
    id: "bats",
    name: "Bats",
    blurb: "A few bats cross the card.",
    glyph: "🦇",
    count: 6,
    duration: 7,
    motion: "drift-across",
  },
  {
    id: "snow",
    name: "Snow",
    blurb: "Slow snow, falling.",
    glyph: "❄",
    count: 14,
    duration: 9,
    motion: "fall",
    particleStyle: { color: "#eaf4ff", textShadow: "0 0 4px rgba(255,255,255,.7)" },
  },
  {
    id: "embers",
    name: "Embers",
    blurb: "Sparks drifting upward.",
    glyph: "•",
    count: 18,
    duration: 6,
    motion: "rise",
    particleStyle: { color: "#ffb347", textShadow: "0 0 6px #ff7a1a", fontSize: 14 },
  },
  {
    id: "sparkle",
    name: "Sparkle",
    blurb: "Quiet glints, here and there.",
    glyph: "✦",
    count: 12,
    duration: 4,
    motion: "twinkle",
    particleStyle: { color: "#fff3a3", textShadow: "0 0 6px #ffd93b" },
  },
];

export function findEffect(id: string | null | undefined): ProfileEffectDef | null {
  if (!id) return null;
  return PROFILE_EFFECTS.find((e) => e.id === id) ?? null;
}

/** A deterministic 0..1 from a seed, so a given particle keeps its position
 *  across re-renders instead of jumping every time React redraws the card. */
function rand(seed: number): number {
  const x = Math.sin(seed * 12.9898) * 43758.5453;
  return x - Math.floor(x);
}

export default function ProfileEffect({
  effectId,
  /** Off when the viewer has asked for no effects, or the OS has. Passed in
   *  rather than read here so the decision lives with the card that knows the
   *  viewer's settings. */
  enabled,
}: {
  effectId: string | null | undefined;
  enabled: boolean;
}) {
  const def = findEffect(effectId);

  const particles = useMemo(() => {
    if (!def) return [];
    return Array.from({ length: def.count }, (_, i) => {
      const a = rand(i + 1);
      const b = rand(i + 101);
      const c = rand(i + 201);
      return {
        key: i,
        left: `${Math.round(a * 92)}%`,
        top: `${Math.round(b * 84)}%`,
        delay: `${(c * def.duration).toFixed(2)}s`,
        duration: `${(def.duration * (0.75 + b * 0.5)).toFixed(2)}s`,
        scale: 0.7 + c * 0.7,
      };
    });
  }, [def]);

  if (!def || !enabled) return null;

  return (
    <div
      aria-hidden="true"
      style={{
        position: "absolute",
        inset: 0,
        overflow: "hidden",
        pointerEvents: "none",
        borderRadius: "inherit",
      }}
    >
      {particles.map((p) => (
        <span
          key={p.key}
          style={{
            position: "absolute",
            left: p.left,
            top: p.top,
            fontSize: 13,
            lineHeight: 1,
            opacity: 0.85,
            transform: `scale(${p.scale})`,
            animation: `farder-effect-${def.motion} ${p.duration} linear ${p.delay} infinite`,
            ...def.particleStyle,
          }}
        >
          {def.glyph}
        </span>
      ))}
    </div>
  );
}
