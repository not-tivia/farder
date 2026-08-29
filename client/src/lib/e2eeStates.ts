/**
 * Turning encryption failures into things a person can act on (sub-5b G3).
 *
 * Every state below is a real, reachable condition of an E2EE channel, and each
 * one arrives at the UI as a raw error string from the Rust side — which is what
 * the user saw before this file existed. The mapping is deliberately explicit
 * rather than clever: each entry names the underlying mechanism, says what it
 * means in plain language, and where possible offers the ONE action that fixes
 * it.
 *
 * Matching is on substrings of the fold's / crate's own messages. That coupling
 * is real and worth stating: if one of those strings changes, this degrades to
 * the generic case rather than misinforming, because every branch here is
 * additive and the fallback shows the raw text.
 */

export type E2eeRepair = "refresh-keys" | "unlock" | "owner-rebuild";

export interface E2eeStateInfo {
  /** Short, human. Never the raw error. */
  title: string;
  /** What it means and what happens next. */
  detail: string;
  /** The one action that resolves it, if there is one. */
  repair?: E2eeRepair;
  /** True when nothing the user does in this channel will help. */
  terminal?: boolean;
}

export function describeE2eeFailure(raw: string): E2eeStateInfo {
  const s = raw.toLowerCase();

  // The freshness ceiling. The channel stopped accepting messages because too
  // much has happened since the last key refresh. Normally the client refreshes
  // by itself (5b-1 K1/K2), so reaching the user means the automatic attempt
  // also failed — hence a manual button rather than "try again".
  if (s.includes("freshness ceiling")) {
    return {
      title: "This channel needs a key refresh",
      detail:
        "Encrypted channels rotate their keys regularly so that anyone who leaves cannot read what comes next. This one is due, and the automatic refresh did not go through. Refreshing takes a moment and changes nothing you can see.",
      repair: "refresh-keys",
    };
  }

  // Drift: a removed/revoked device still holds a leaf, which seals the channel
  // until someone clears it.
  if (s.includes("pending removals") || s.includes("pending_removals")) {
    return {
      title: "A removed device still needs clearing",
      detail:
        "Someone was banned, or a device was retired, and the channel is holding until their key is cleared out. This is the channel refusing to send anything the removed device could still read. Refreshing the keys clears it.",
      repair: "refresh-keys",
    };
  }

  // F4 terminal: an impostor leaf was merged and cannot be rolled back.
  if (s.includes("poisoned") || s.includes("could not be confirmed")) {
    return {
      title: "This channel's encryption could not be verified",
      detail:
        "The app found a device key that does not match what the server's records say it should be, and it will not send anything more into this channel. This is the fail-safe working, not a glitch. The channel owner needs to rebuild the channel before it can be used again.",
      repair: "owner-rebuild",
      terminal: true,
    };
  }

  // The identity is locked (the archive key and the device key both hang off it).
  if (s.includes("identity is locked")) {
    return {
      title: "Locked",
      detail: "Unlock with your PIN to read and send encrypted messages.",
      repair: "unlock",
    };
  }

  // The MLS store is gone / cloned / rolled back. This is the honest one: the
  // 5a carry-forwards say a single-device identity CANNOT self-recover, and the
  // user must be told rather than left to wonder.
  if (s.includes("store resume is terminal") || s.includes("store instance")) {
    return {
      title: "This device lost its encryption keys for this channel",
      detail:
        "The keys this device used for this channel are gone or no longer trustworthy, so it cannot read the channel. If you have another device signed in, it can bring this one back in — but messages sent before now stay unreadable on this device. If this is your only device, the channel owner has to rebuild the channel.",
      repair: "owner-rebuild",
      terminal: true,
    };
  }

  // Rate-limited rekey: a policy refusal, not a fault. It resolves by waiting.
  if (s.includes("commit-rate rule") || s.includes("not permitted yet")) {
    return {
      title: "Keys were refreshed very recently",
      detail:
        "The server limits how often a channel's keys can change, to stop that being used as a nuisance. Wait a moment and send again.",
    };
  }

  // We have not confirmed our own leaf yet — the "waiting for keys" case, which
  // is normal for a member who just joined.
  if (s.includes("before this device's leaf is confirmed")) {
    return {
      title: "Waiting for keys",
      detail:
        "You will be able to send once a member who already holds this channel's keys is online to add you. This usually takes a few seconds.",
    };
  }

  if (s.includes("over cap")) {
    return {
      title: "Message too long",
      detail: "Encrypted messages have a size limit. Shorten it and try again.",
    };
  }

  // Unknown: show the raw text rather than inventing an explanation. A wrong
  // explanation is worse than an ugly one.
  return { title: "Couldn't send", detail: raw };
}
