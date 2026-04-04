import { useState } from "react";
import { useServer } from "../context/ServerContext";
import * as api from "../lib/tauri-bridge";

interface Props {
  onClose: () => void;
}

type Tab = "channel" | "category";

export default function ServerSettingsDialog({ onClose }: Props) {
  const { state } = useServer();
  const [activeTab, setActiveTab] = useState<Tab>("channel");

  // Create Channel form
  const [chName, setChName] = useState("");
  const [chType, setChType] = useState("Text");
  const [chCategoryId, setChCategoryId] = useState<number | undefined>(undefined);
  const [chLoading, setChLoading] = useState(false);
  const [chError, setChError] = useState<string | null>(null);
  const [chSuccess, setChSuccess] = useState(false);

  // Create Category form
  const [catName, setCatName] = useState("");
  const [catLoading, setCatLoading] = useState(false);
  const [catError, setCatError] = useState<string | null>(null);
  const [catSuccess, setCatSuccess] = useState(false);

  async function handleCreateChannel() {
    if (!chName.trim()) return;
    setChLoading(true);
    setChError(null);
    setChSuccess(false);
    try {
      await api.createChannel(chName.trim(), chType, chCategoryId);
      setChSuccess(true);
      setChName("");
      setTimeout(() => setChSuccess(false), 2000);
    } catch (e) {
      setChError(String(e));
    } finally {
      setChLoading(false);
    }
  }

  async function handleCreateCategory() {
    if (!catName.trim()) return;
    setCatLoading(true);
    setCatError(null);
    setCatSuccess(false);
    try {
      await api.createCategory(catName.trim());
      setCatSuccess(true);
      setCatName("");
      setTimeout(() => setCatSuccess(false), 2000);
    } catch (e) {
      setCatError(String(e));
    } finally {
      setCatLoading(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-titlebar">
          <span>Server Settings</span>
          <button className="modal-close" onClick={onClose}>X</button>
        </div>
        <div className="modal-body">
          <div className="settings-tabs">
            <button
              className={`settings-tab${activeTab === "channel" ? " active" : ""}`}
              onClick={() => setActiveTab("channel")}
            >
              Create Channel
            </button>
            <button
              className={`settings-tab${activeTab === "category" ? " active" : ""}`}
              onClick={() => setActiveTab("category")}
            >
              Create Category
            </button>
          </div>

          {activeTab === "channel" && (
            <div className="settings-tab-content">
              <div className="connect-section">
                <label className="connect-label">Channel Name</label>
                <input
                  className="connect-input"
                  value={chName}
                  onChange={(e) => setChName(e.target.value)}
                  placeholder="e.g. general"
                />
              </div>
              <div className="connect-section">
                <label className="connect-label">Type</label>
                <select
                  className="connect-input"
                  value={chType}
                  onChange={(e) => setChType(e.target.value)}
                >
                  <option value="Text">Text</option>
                  <option value="Announcement">Announcement</option>
                </select>
              </div>
              <div className="connect-section">
                <label className="connect-label">Category (optional)</label>
                <select
                  className="connect-input"
                  value={chCategoryId ?? ""}
                  onChange={(e) =>
                    setChCategoryId(e.target.value ? Number(e.target.value) : undefined)
                  }
                >
                  <option value="">None</option>
                  {state.categories.map((cat) => (
                    <option key={cat.id} value={cat.id}>
                      {cat.name}
                    </option>
                  ))}
                </select>
              </div>
              {chError && <div className="error-text">{chError}</div>}
              {chSuccess && <div className="success-text">Channel created!</div>}
              <div className="connect-actions">
                <button
                  className="xp-button"
                  onClick={handleCreateChannel}
                  disabled={chLoading || !chName.trim()}
                >
                  {chLoading ? "Creating..." : "Create Channel"}
                </button>
              </div>
            </div>
          )}

          {activeTab === "category" && (
            <div className="settings-tab-content">
              <div className="connect-section">
                <label className="connect-label">Category Name</label>
                <input
                  className="connect-input"
                  value={catName}
                  onChange={(e) => setCatName(e.target.value)}
                  placeholder="e.g. General"
                />
              </div>
              {catError && <div className="error-text">{catError}</div>}
              {catSuccess && <div className="success-text">Category created!</div>}
              <div className="connect-actions">
                <button
                  className="xp-button"
                  onClick={handleCreateCategory}
                  disabled={catLoading || !catName.trim()}
                >
                  {catLoading ? "Creating..." : "Create Category"}
                </button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
