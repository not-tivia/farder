// An interval that does nothing while the window cannot be seen.
//
// Farder is meant to sit open all day like a messenger. Timers that keep waking
// a minimised window cost battery and keep the machine out of deep sleep for
// nothing: the server already PUSHES changes, so a poll is only a safety net,
// and a countdown nobody can see needs no frames. So: no ticks while hidden, and
// ONE catch-up call the moment it is visible again.
//
// Pure (the environment is passed in) so it can be tested without a DOM.

export interface VisibleEnv {
  isHidden(): boolean;
  /** Subscribe to visibility changes; returns an unsubscribe. */
  onVisibilityChange(cb: () => void): () => void;
  setInterval(cb: () => void, ms: number): number;
  clearInterval(id: number): void;
}

export function browserEnv(): VisibleEnv {
  return {
    isHidden: () => typeof document !== "undefined" && document.visibilityState === "hidden",
    onVisibilityChange: (cb) => {
      document.addEventListener("visibilitychange", cb);
      return () => document.removeEventListener("visibilitychange", cb);
    },
    setInterval: (cb, ms) => window.setInterval(cb, ms),
    clearInterval: (id) => window.clearInterval(id),
  };
}

/**
 * Call `fn` every `ms` while visible. Hidden: the timer is stopped entirely (no
 * wakeups at all). Becoming visible: `fn` runs once immediately, then the timer
 * restarts. Returns a stop function.
 */
export function visibleInterval(fn: () => void, ms: number, env: VisibleEnv = browserEnv()): () => void {
  let timer: number | null = null;
  const start = () => { if (timer === null) timer = env.setInterval(fn, ms); };
  const stop = () => { if (timer !== null) { env.clearInterval(timer); timer = null; } };

  if (!env.isHidden()) start();
  const unsubscribe = env.onVisibilityChange(() => {
    if (env.isHidden()) {
      stop();
    } else if (timer === null) {
      fn(); // catch up on whatever was missed
      start();
    }
  });
  return () => { stop(); unsubscribe(); };
}
