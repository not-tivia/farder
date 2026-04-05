import { useState, useEffect } from "react";
import * as api from "../lib/tauri-bridge";
import type { FavoriteEntry } from "../lib/tauri-bridge";

interface Props {
  onSelect: (favorite: FavoriteEntry) => void;
  onClose: () => void;
}

export default function FavoritesPanel({ onSelect, onClose }: Props) {
  const [favorites, setFavorites] = useState<FavoriteEntry[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api.listFavorites().then(f => {
      setFavorites(f);
      setLoading(false);
    }).catch(() => setLoading(false));
  }, []);

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
      <div className="favorites-grid">
        {loading && <div className="favorites-empty">Loading...</div>}
        {!loading && favorites.length === 0 && (
          <div className="favorites-empty">No favorites yet. Click an image and select "Favorite" to add one!</div>
        )}
        {favorites.map(fav => (
          <div key={fav.id} className="favorite-item" onClick={() => onSelect(fav)} title={`From ${fav.source_server}`}>
            <img src={fav.data_url} alt={fav.file_name} />
            <button className="favorite-remove" onClick={(e) => handleRemove(fav.id, e)} title="Remove">x</button>
          </div>
        ))}
      </div>
    </div>
  );
}
