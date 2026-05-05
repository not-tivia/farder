import { invoke } from "@tauri-apps/api/core";

export interface TenorGif {
  id: string;
  title: string;
  preview_url: string;
  full_url: string;
  width: number;
  height: number;
}

export interface TenorSearchResult {
  gifs: TenorGif[];
  next: string | null;
}

export interface GifSearchSettings {
  enabled: boolean;
  content_filter: "high" | "medium" | "low" | "off";
  user_api_key: string | null;
}

export async function tenorSearch(query: string, pos?: string): Promise<TenorSearchResult> {
  return invoke<TenorSearchResult>("tenor_search", { query, pos: pos ?? null });
}

export async function tenorTrending(pos?: string): Promise<TenorSearchResult> {
  return invoke<TenorSearchResult>("tenor_trending", { pos: pos ?? null });
}

export async function getGifSearchSettings(): Promise<GifSearchSettings> {
  return invoke<GifSearchSettings>("get_gif_search_settings");
}

export async function setGifSearchSettings(settings: GifSearchSettings): Promise<void> {
  return invoke<void>("set_gif_search_settings", { settings });
}
