import type { Presence } from "./types";

/** Render a presence as a single line, e.g. "🎵 Listening to Song – Artist".
 * Test-notes (by inspection):
 *   {Music,"S","A"} -> "🎵 Listening to S – A"
 *   {Music,"S",null} -> "🎵 Listening to S"
 *   {Game,"Valorant",null} -> "🎮 Playing Valorant"
 */
export function formatPresence(p: Presence): string {
  if (p.kind === "Music") {
    return p.state ? `🎵 Listening to ${p.details} – ${p.state}` : `🎵 Listening to ${p.details}`;
  }
  return `🎮 Playing ${p.details}`;
}
