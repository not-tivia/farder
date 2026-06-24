import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import {
  type DataSaverSettings,
  DATA_SAVER_DEFAULTS,
  getDataSaver,
  setDataSaver,
  hasDataSaver,
} from "../lib/dataSaver";
import { getDataSaverEmbeds } from "../lib/tauri-bridge";

interface DataSaverCtx {
  settings: DataSaverSettings;
  update: (patch: Partial<DataSaverSettings>) => void;
}

const Ctx = createContext<DataSaverCtx>({
  settings: DATA_SAVER_DEFAULTS,
  update: () => {},
});

export function DataSaverProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<DataSaverSettings>(() => getDataSaver());

  // One-time migration from the legacy Rust `data_saver_embeds` setting:
  // only runs when nothing is stored locally yet.
  useEffect(() => {
    if (hasDataSaver()) return;
    getDataSaverEmbeds()
      .then((on) => {
        const seeded = { ...DATA_SAVER_DEFAULTS, enabled: on, clickToLoadEmbeds: on };
        setDataSaver(seeded);
        setSettings(seeded);
      })
      .catch(() => { /* defaults stand */ });
  }, []);

  const update = (patch: Partial<DataSaverSettings>) => {
    setSettings((prev) => {
      const next = { ...prev, ...patch };
      setDataSaver(next);
      return next;
    });
  };

  return <Ctx.Provider value={{ settings, update }}>{children}</Ctx.Provider>;
}

export function useDataSaver(): DataSaverCtx {
  return useContext(Ctx);
}
