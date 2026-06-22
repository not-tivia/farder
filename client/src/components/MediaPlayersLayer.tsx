import { useMediaPlayers } from "../context/MediaPlayersContext";
import MediaPlayer from "./MediaPlayer";

// Renders every active media player once, at the app-root overlay level, so a
// player is never re-parented (no reload) and floating players persist across
// channel/server navigation.
export default function MediaPlayersLayer() {
  const { players } = useMediaPlayers();
  if (players.length === 0) return null;
  return <>{players.map((p) => <MediaPlayer key={p.id} player={p} />)}</>;
}
