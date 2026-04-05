import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import type { FavoriteEntry } from "../lib/tauri-bridge";

interface Props {
  onSelect: (favorite: FavoriteEntry) => void;
  onClose: () => void;
}

function formatFavDate(ts: number): string {
  try {
    return new Date(ts * 1000).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
  } catch {
    return "";
  }
}

export default function FavoritesPanel({ onSelect, onClose }: Props) {
  const [favorites, setFavorites] = useState<FavoriteEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    api.listFavorites().then(f => {
      setFavorites(f);
      setLoading(false);
    }).catch(() => setLoading(false));
  }, []);

  const filteredFavorites = favorites.filter(f =>
    !filter || f.file_name.toLowerCase().includes(filter.toLowerCase())
  );

  async function handleRemove(id: string, e: React.MouseEvent) {
    e.stopPropagation();
    try {
      await api.removeFavorite(id);
      setFavorites(prev => prev.filter(f => f.id !== id));
    } catch {}
  }

  return (
    <div className="favorites-panel">
      <div className="favorites-header">
        <span>Favorites</span>
        <button className="modal-close" onClick={onClose}>X</button>
      </div>
      <div className="favorites-search">
        <input
          className="favorites-search-input"
          value={filter}
          onChange={e => setFilter(e.target.value)}
          placeholder="Search stickers..."
        />
      </div>
      <div className="favorites-grid">
        {loading && <div className="favorites-empty">Loading...</div>}
        {!loading && filteredFavorites.length === 0 && (
          <div className="favorites-empty">
            {filter ? "No matches." : "No favorites yet. Click an image and select \"Favorite\" to add one!"}
          </div>
        )}
        {filteredFavorites.map(fav => (
          <div
            key={fav.id}
            className="favorite-item"
            onClick={() => onSelect(fav)}
            title={`${fav.file_name}\nFrom: ${fav.source_server}\nAdded: ${formatFavDate(fav.favorited_at)}`}
          >
            <img src={fav.data_url} alt={fav.file_name} />
            <button className="favorite-remove" onClick={(e) => handleRemove(fav.id, e)} title="Remove">x</button>
          </div>
        ))}
      </div>
    </div>
  );
}
