import type { RegionId, RegionState, ImageFit } from "./types";
import { REGIONS } from "./regions";

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
