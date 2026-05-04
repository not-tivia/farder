export type RegionId =
  | "main-bg"
  | "channel-sidebar"
  | "server-strip"
  | "member-sidebar"
  | "title-bars"
  | "message-bubble"
  | "message-hover"
  | "buttons"
  | "input-field"
  | "modal-bg"
  | "scrollbars"
  | "accent";

export type ImageFit = "stretch" | "tile" | "center" | "cover";

export interface RegionImage {
  /** Relative path within the theme folder, e.g. "./assets/dog.jpg" */
  path: string;
  fit: ImageFit;
}

export interface RegionState {
  bgColor?: string;
  bgImage?: RegionImage;
  textColor?: string;
}

export interface RegionDefinition {
  id: RegionId;
  label: string;
  /** Whether this region accepts a text-color knob (some, like scrollbars, don't). */
  hasText: boolean;
  /** Whether this region accepts a background image (scrollbars, accent don't really). */
  hasImage: boolean;
  /** CSS selectors targeted for the background. */
  backgroundSelectors: string[];
  /** CSS selectors targeted for the text color. Empty if hasText is false. */
  textSelectors: string[];
  /** If set, this region's color drives a CSS variable instead of selector rules.
   *  Used by 'accent' which sets --xp-blue. */
  accentVariable?: string;
}

export interface CustomizerSession {
  themeId: string;
  baseThemeId: string;
  regions: Map<RegionId, RegionState>;
  history: Array<Map<RegionId, RegionState>>;
  historyIndex: number;
  dirty: boolean;
}
