import type { RegionId, RegionState, ImageFit } from "./types";
import { REGIONS } from "./regions";

// Build a label -> region lookup once at module load (labels are unique per regions.ts).
const REGION_BY_LABEL: Map<string, (typeof REGIONS)[number]> = new Map(
  REGIONS.map((r) => [r.label, r]),
);

const MARKER = "/* === Customizer overrides — generated, edit with the customizer === */";

function imageDeclaration(path: string, fit: ImageFit): string {
  const url = `url('${path.replace(/'/g, "\\'")}')`;
  switch (fit) {
    case "stretch":
      return `background: ${url} no-repeat; background-size: 100% 100%;`;
    case "tile":
      return `background: ${url} repeat; background-size: auto;`;
    case "center":
      return `background: ${url} no-repeat center center; background-size: auto;`;
    case "cover":
      return `background: ${url} no-repeat center center; background-size: cover;`;
  }
}

function escapeSelector(s: string): string {
  // Selectors come from a fixed map (regions.ts) — no untrusted input.
  // This function exists so future contributors don't accidentally
  // interpolate user-controlled strings into selectors. Keep as identity for now.
  return s;
}

/**
 * Build the full overrides CSS for the given region states. Returns the empty
 * string if no region has any override set. Pure function — no DOM access.
 */
export function generateOverrideCss(regions: Map<RegionId, RegionState>): string {
  const blocks: string[] = [];
  let hasAny = false;

  for (const region of REGIONS) {
    const state = regions.get(region.id);
    if (!state || (state.bgColor === undefined && state.bgImage === undefined && state.textColor === undefined)) {
      continue;
    }
    hasAny = true;

    // Special case: accent is just a CSS variable change.
    if (region.accentVariable && state.bgColor) {
      blocks.push(`:root { ${region.accentVariable}: ${state.bgColor}; }`);
      continue;
    }

    const decls: string[] = [];
    if (state.bgImage) {
      decls.push(imageDeclaration(state.bgImage.path, state.bgImage.fit));
    } else if (state.bgColor) {
      decls.push(`background: ${state.bgColor};`);
    }
    if (decls.length > 0 && region.backgroundSelectors.length > 0) {
      const selectorList = region.backgroundSelectors.map(escapeSelector).join(", ");
      blocks.push(`/* ${region.label} — background */\n${selectorList} { ${decls.join(" ")} }`);
    }

    if (state.textColor && region.hasText && region.textSelectors.length > 0) {
      const selectorList = region.textSelectors.map(escapeSelector).join(", ");
      blocks.push(`/* ${region.label} — text */\n${selectorList} { color: ${state.textColor}; }`);
    }
  }

  if (!hasAny) return "";
  return [MARKER, ...blocks].join("\n\n") + "\n";
}

/**
 * Strip any previously-generated customizer overrides from a CSS string,
 * leaving everything before the marker. Used when saving to disk so we don't
 * accumulate stale override blocks across saves.
 */
export function stripExistingOverrides(css: string): string {
  const idx = css.indexOf(MARKER);
  if (idx === -1) return css;
  return css.slice(0, idx).trimEnd() + "\n";
}

/** Combine base CSS + overrides into the final theme.css to write to disk. */
export function mergeForSave(baseCss: string, overrideCss: string): string {
  const stripped = stripExistingOverrides(baseCss);
  if (!overrideCss) return stripped;
  return stripped + "\n" + overrideCss;
}

/**
 * Parse a CSS string produced by generateOverrideCss back into a Map of
 * RegionId -> RegionState. Symmetric with generateOverrideCss: every value
 * this function produces round-trips through generateOverrideCss unchanged.
 *
 * If the CSS contains no customizer marker block, returns an empty map.
 * Regions absent from the CSS simply do not appear in the output map —
 * that is not an error (it means "unset", matching the empty-map starting
 * state the customizer already uses for new themes).
 */
export function parseOverrideCss(css: string): Map<RegionId, RegionState> {
  const out = new Map<RegionId, RegionState>();

  const markerIdx = css.indexOf(MARKER);
  if (markerIdx === -1) return out;

  // Only look at the portion after the marker.
  const block = css.slice(markerIdx + MARKER.length);

  // ---- Accent region ---------------------------------------------------------
  // Format: :root { --xp-blue: <color>; }
  // We match based on the accentVariable defined per region rather than
  // hard-coding --xp-blue, so future accent regions work automatically.
  for (const region of REGIONS) {
    if (!region.accentVariable) continue;
    // Escape the variable name for use in a regex (-- is safe, letters/digits/-).
    const varEscaped = region.accentVariable.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const accentRe = new RegExp(
      ":root\\s*\\{[^}]*" + varEscaped + ":\\s*([^;}]+?)\\s*;[^}]*\\}",
    );
    const m = accentRe.exec(block);
    if (m) {
      const existing = out.get(region.id as RegionId) ?? {};
      out.set(region.id as RegionId, { ...existing, bgColor: m[1].trim() });
    }
  }

  // ---- Label-keyed blocks ----------------------------------------------------
  // Format: /* <label> — background */\n<selectors> { <decls> }
  //     and  /* <label> — text */\n<selectors> { color: <color>; }
  //
  // The em-dash is U+2014.  We match the comment, then grab the rule body
  // up to the first closing brace that follows it.
  //
  // Regex breakdown:
  //   /\/\* (.+?) — (background|text) \*\/\s*[^{]*\{([^}]*)\}/g
  //   group 1 = label, group 2 = "background" or "text", group 3 = declaration block
  const blockRe = /\/\* (.+?) — (background|text) \*\/\s*[^{]*\{([^}]*)\}/g;
  let m: RegExpExecArray | null;

  while ((m = blockRe.exec(block)) !== null) {
    const label = m[1].trim();
    const kind = m[2] as "background" | "text";
    const decls = m[3].trim();

    const region = REGION_BY_LABEL.get(label);
    if (!region) continue; // unknown label — skip gracefully

    const rid = region.id as RegionId;
    const existing = out.get(rid) ?? {};

    if (kind === "text") {
      // Format: color: <value>;
      const colorMatch = /color:\s*([^;]+);/.exec(decls);
      if (colorMatch) {
        out.set(rid, { ...existing, textColor: colorMatch[1].trim() });
      }
      continue;
    }

    // kind === "background"
    // Check for an image first (url(...) present).
    const urlMatch = /url\('([^']+)'\)/.exec(decls);
    if (urlMatch) {
      const path = urlMatch[1].replace(/\\'/g, "'");
      const fit = detectImageFit(decls);
      out.set(rid, { ...existing, bgImage: { path, fit } });
      continue;
    }

    // Plain color: background: <value>;
    const bgColorMatch = /background:\s*([^;]+);/.exec(decls);
    if (bgColorMatch) {
      out.set(rid, { ...existing, bgColor: bgColorMatch[1].trim() });
    }
  }

  return out;
}

/**
 * Detect the ImageFit value from a CSS declaration block produced by
 * imageDeclaration(). Defaults to "cover" if the pattern is unrecognised.
 */
function detectImageFit(decls: string): ImageFit {
  // stretch:  background-size: 100% 100%;
  if (/background-size:\s*100%\s*100%/.test(decls)) return "stretch";
  // tile:     background: url(...) repeat;  background-size: auto;
  if (/\)\s*repeat\b/.test(decls)) return "tile";
  // center:   url(...) no-repeat center center; background-size: auto;
  // cover:    url(...) no-repeat center center; background-size: cover;
  if (/background-size:\s*cover/.test(decls)) return "cover";
  if (/no-repeat\s+center\s+center/.test(decls) && /background-size:\s*auto/.test(decls)) return "center";
  // Default to cover (most common / safe fallback).
  return "cover";
}
