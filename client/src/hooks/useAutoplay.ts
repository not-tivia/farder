import { useEffect, useState } from "react";
import { useDataSaver } from "../context/DataSaverContext";
import {
  AUTOPLAY_CHANGED, getAutoplay, prefersReducedMotion, shouldAutoplay, type AutoplaySettings,
} from "../lib/autoplay";

/** The autoplay settings, kept current when they change anywhere in the app. */
export function useAutoplaySettings(): AutoplaySettings {
  const [s, setS] = useState<AutoplaySettings>(getAutoplay);
  useEffect(() => {
    const onChange = () => setS(getAutoplay());
    window.addEventListener(AUTOPLAY_CHANGED, onChange);
    return () => window.removeEventListener(AUTOPLAY_CHANGED, onChange);
  }, []);
  return s;
}

/**
 * Whether media from `authorPk` may start by itself right now: autoplay is on,
 * they are on my trusted list, Data Saver is off and the OS is not asking for
 * reduced motion. See `shouldAutoplay`.
 */
export function useAutoplayFor(authorPk: string | null | undefined): boolean {
  const s = useAutoplaySettings();
  const { settings } = useDataSaver();
  return shouldAutoplay(s, authorPk, { dataSaverOn: settings.enabled, reducedMotion: prefersReducedMotion() });
}
