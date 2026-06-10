import { useEffect, useRef, useState, type ReactNode } from "react";
import * as api from "../lib/tauri-bridge";

type Screen = "loading" | "set-pin" | "enter-pin" | "migrate" | "restore" | "show-phrase";

// Render the 24-word recovery phrase to a PNG and return its raw base64 (no
// data-URL prefix). The image bakes in a "keep secret" warning so an exported
// copy carries its own caution. Returns null if a canvas context isn't
// available.
function renderPhraseImage(phrase: string): string | null {
  const words = phrase.trim().split(/\s+/);
  const cols = 3;
  const rows = Math.ceil(words.length / cols);
  const W = 900;
  const padX = 60;
  const headerH = 150;
  const rowH = 56;
  const H = headerH + rows * rowH + 80;

  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;

  // Background.
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, W, H);

  // Header.
  ctx.fillStyle = "#111111";
  ctx.font = "bold 34px sans-serif";
  ctx.textBaseline = "top";
  ctx.fillText("Farder Recovery Phrase", padX, 36);

  // Warning, baked into the image.
  ctx.fillStyle = "#c0392b";
  ctx.font = "bold 20px sans-serif";
  ctx.fillText("KEEP SECRET - anyone with these words can take over your account.", padX, 84);
  ctx.fillStyle = "#666666";
  ctx.font = "16px sans-serif";
  ctx.fillText("Store offline (USB or printed). Do not leave this image on a shared drive.", padX, 112);

  // Word grid, numbered.
  const colW = (W - padX * 2) / cols;
  ctx.font = "22px monospace";
  for (let i = 0; i < words.length; i++) {
    const col = i % cols;
    const row = Math.floor(i / cols);
    const x = padX + col * colW;
    const y = headerH + row * rowH;
    ctx.fillStyle = "#999999";
    const num = `${i + 1}`.padStart(2, "0");
    ctx.fillText(`${num}.`, x, y);
    ctx.fillStyle = "#111111";
    ctx.fillText(words[i], x + 46, y);
  }

  const dataUrl = canvas.toDataURL("image/png");
  const comma = dataUrl.indexOf(",");
  return comma >= 0 ? dataUrl.slice(comma + 1) : null;
}

// Reusable 4-digit PIN field rendered as 4 circles (Twitter/X DM-unlock style).
// Keeps the same value/onChange contract (value is a 0-4 char digit string).
// A visually-hidden <input> handles all keystrokes; clicking/tapping the circle
// row focuses it. onComplete fires as soon as the 4th digit is entered, or on
// Enter when 4 digits are already present.
function PinField({
  value,
  onChange,
  autoFocus,
  onComplete,
}: {
  value: string;
  onChange: (v: string) => void;
  autoFocus?: boolean;
  onComplete?: () => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const next = e.target.value.replace(/\D/g, "").slice(0, 4);
    onChange(next);
    if (next.length === 4) onComplete?.();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && value.length === 4) onComplete?.();
  };

  const circleStyle = (filled: boolean): React.CSSProperties => ({
    width: 48,
    height: 48,
    borderRadius: "50%",
    border: filled ? "none" : "2px solid var(--xp-blue, #5865F2)",
    background: filled ? "var(--xp-blue, #5865F2)" : "transparent",
    boxSizing: "border-box",
    transition: "background 0.12s ease, border 0.12s ease",
    flexShrink: 0,
  });

  return (
    <div
      style={{ position: "relative", display: "flex", gap: 16, justifyContent: "center", cursor: "text", margin: "12px 0" }}
      onClick={() => inputRef.current?.focus()}
    >
      {[0, 1, 2, 3].map((i) => (
        <div key={i} style={circleStyle(i < value.length)} />
      ))}
      {/* Visually hidden but focusable input that captures all keystrokes */}
      <input
        ref={inputRef}
        type="password"
        inputMode="numeric"
        maxLength={4}
        autoFocus={autoFocus}
        value={value}
        onChange={handleChange}
        onKeyDown={handleKeyDown}
        style={{
          position: "absolute",
          inset: 0,
          opacity: 0,
          width: "100%",
          height: "100%",
          cursor: "text",
          zIndex: 1,
        }}
        aria-label="PIN entry"
      />
    </div>
  );
}

export default function IdentityGate({ onUnlocked }: { onUnlocked: () => void }) {
  const [screen, setScreen] = useState<Screen>("loading");
  const [pin, setPin] = useState("");
  const [pin2, setPin2] = useState("");
  // Used to programmatically focus the confirm-PIN circle field after the
  // first field fills.  We toggle this boolean to give the confirm PinField
  // a fresh autoFocus when the choose-field completes.
  const [confirmFocus, setConfirmFocus] = useState(false);
  const [phrase, setPhrase] = useState("");
  const [recoveryPhrase, setRecoveryPhrase] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [saveNote, setSaveNote] = useState<string | null>(null);

  useEffect(() => {
    api
      .identityStatus()
      .then((s) =>
        setScreen(s === "none" ? "set-pin" : s === "plaintext" ? "migrate" : "enter-pin"),
      )
      .catch(() => setScreen("set-pin"));
  }, []);

  const reset = () => {
    setPin("");
    setPin2("");
    setError(null);
    setConfirmFocus(false);
  };

  async function handleCreate() {
    if (pin.length !== 4) return setError("PIN must be 4 digits.");
    if (pin !== pin2) return setError("PINs do not match.");
    setBusy(true);
    setError(null);
    try {
      const res = await api.createIdentity(pin);
      setRecoveryPhrase(res.recovery_phrase);
      setScreen("show-phrase");
    } catch (e) {
      setError("Could not create identity. Please try again.");
      console.error("[identity] create failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleMigrate() {
    if (pin.length !== 4) return setError("PIN must be 4 digits.");
    if (pin !== pin2) return setError("PINs do not match.");
    setBusy(true);
    setError(null);
    try {
      const res = await api.migratePlaintextIdentity(pin);
      setRecoveryPhrase(res.recovery_phrase);
      setScreen("show-phrase");
    } catch (e) {
      setError("Could not secure your account. Please try again.");
      console.error("[identity] migrate failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleUnlock() {
    setBusy(true);
    setError(null);
    try {
      await api.unlockIdentity(pin);
      onUnlocked();
    } catch (e) {
      setError("Incorrect PIN.");
      setPin("");
      console.error("[identity] unlock failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleRestore() {
    if (pin.length !== 4) return setError("New PIN must be 4 digits.");
    setBusy(true);
    setError(null);
    try {
      await api.restoreIdentity(phrase.trim(), pin);
      onUnlocked();
    } catch (e) {
      setError("That recovery phrase or PIN was not accepted.");
      console.error("[identity] restore failed:", e);
    } finally {
      setBusy(false);
    }
  }

  async function handleCopyPhrase() {
    try {
      await navigator.clipboard.writeText(recoveryPhrase);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
      // Best-effort: clear the clipboard ~60s later so the seed doesn't linger
      // in clipboard history, but only if it still holds our phrase.
      setTimeout(() => {
        navigator.clipboard
          .readText()
          .then((cur) => {
            if (cur === recoveryPhrase) return navigator.clipboard.writeText("");
          })
          .catch(() => {});
      }, 60000);
    } catch (e) {
      setSaveNote("Couldn't copy - select the words and copy manually.");
      console.error("[identity] copy failed:", e);
    }
  }

  async function handleSaveImage() {
    setSaveNote(null);
    const b64 = renderPhraseImage(recoveryPhrase);
    if (!b64) {
      setSaveNote("Couldn't render the image on this device.");
      return;
    }
    try {
      const saved = await api.saveRecoveryImage(b64);
      setSaveNote(saved ? "Saved. Keep that file somewhere safe and offline." : null);
    } catch (e) {
      setSaveNote("Couldn't save the image.");
      console.error("[identity] save image failed:", e);
    }
  }

  const shell = (title: string, body: ReactNode) => (
    <div className="connect-screen">
      <div className="connect-dialog">
        <div className="connect-dialog-titlebar">{title}</div>
        <div className="connect-dialog-body" style={{ padding: 24 }}>
          {body}
          {error && <p style={{ color: "var(--danger, #d9534f)", marginTop: 8 }}>{error}</p>}
        </div>
      </div>
    </div>
  );

  if (screen === "loading") return shell("Farder", <p>Loading...</p>);

  if (screen === "set-pin")
    return shell(
      "Set a PIN",
      <>
        <p>Choose a 4-digit PIN. You'll enter it each time you open Farder. It encrypts your identity on this device.</p>
        <p style={{ textAlign: "center", color: "var(--text-muted, #888)", fontSize: 13, margin: "0 0 4px" }}>Choose PIN</p>
        <PinField
          value={pin}
          onChange={(v) => { setPin(v); if (v.length < 4) setConfirmFocus(false); }}
          autoFocus
          onComplete={() => setConfirmFocus(true)}
        />
        <p style={{ textAlign: "center", color: "var(--text-muted, #888)", fontSize: 13, margin: "0 0 4px" }}>Confirm PIN</p>
        <PinField
          value={pin2}
          onChange={setPin2}
          autoFocus={confirmFocus}
          onComplete={handleCreate}
        />
        <button className="connect-button" disabled={busy} onClick={handleCreate}>
          {busy ? "Creating..." : "Create identity"}
        </button>
        <button className="connect-link" onClick={() => { reset(); setScreen("restore"); }}>
          Restore from recovery phrase
        </button>
      </>,
    );

  if (screen === "migrate")
    return shell(
      "Secure your account",
      <>
        <p>Your identity key was stored unprotected. Set a 4-digit PIN now to encrypt it on this device.</p>
        <p style={{ textAlign: "center", color: "var(--text-muted, #888)", fontSize: 13, margin: "0 0 4px" }}>Choose PIN</p>
        <PinField
          value={pin}
          onChange={(v) => { setPin(v); if (v.length < 4) setConfirmFocus(false); }}
          autoFocus
          onComplete={() => setConfirmFocus(true)}
        />
        <p style={{ textAlign: "center", color: "var(--text-muted, #888)", fontSize: 13, margin: "0 0 4px" }}>Confirm PIN</p>
        <PinField
          value={pin2}
          onChange={setPin2}
          autoFocus={confirmFocus}
          onComplete={handleMigrate}
        />
        <button className="connect-button" disabled={busy} onClick={handleMigrate}>
          {busy ? "Securing..." : "Secure account"}
        </button>
      </>,
    );

  if (screen === "enter-pin")
    return shell(
      "Enter your PIN",
      <>
        <PinField value={pin} onChange={setPin} autoFocus onComplete={handleUnlock} />
        <button
          className="connect-button"
          disabled={busy || pin.length !== 4}
          onClick={handleUnlock}
        >
          {busy ? "Unlocking..." : "Unlock"}
        </button>
        <button className="connect-link" onClick={() => { reset(); setScreen("restore"); }}>
          Forgot PIN? Restore from recovery phrase
        </button>
      </>,
    );

  if (screen === "restore")
    return shell(
      "Restore from recovery phrase",
      <>
        <p>Enter your 24-word recovery phrase and choose a new 4-digit PIN.</p>
        <textarea
          className="connect-input"
          rows={3}
          placeholder="word1 word2 word3 ..."
          value={phrase}
          onChange={(e) => setPhrase(e.target.value)}
        />
        <p style={{ textAlign: "center", color: "var(--text-muted, #888)", fontSize: 13, margin: "0 0 4px" }}>New PIN</p>
        <PinField value={pin} onChange={setPin} onComplete={handleRestore} />
        <button className="connect-button" disabled={busy} onClick={handleRestore}>
          {busy ? "Restoring..." : "Restore"}
        </button>
        <button className="connect-link" onClick={() => { reset(); setScreen("enter-pin"); }}>
          Back
        </button>
      </>,
    );

  // show-phrase
  return shell(
    "Save your recovery phrase",
    <>
      <p>
        <strong>Write these 24 words down and keep them safe.</strong> They are the
        only way to recover your account if you forget your PIN. Anyone with this
        phrase can access your account.
      </p>
      <p style={{ fontFamily: "monospace", background: "var(--input-bg, #00000022)", padding: 12, borderRadius: 6, wordSpacing: 4 }}>
        {recoveryPhrase}
      </p>
      <div style={{ display: "flex", gap: 8 }}>
        <button className="connect-link" onClick={handleCopyPhrase}>
          {copied ? "Copied!" : "Copy"}
        </button>
        <button className="connect-link" onClick={handleSaveImage}>
          Save as image
        </button>
      </div>
      <p style={{ fontSize: 12, color: "var(--text-muted, #888)", marginTop: 4 }}>
        A copy or image is an unencrypted backup of your account key. Prefer
        offline storage (USB or printed); don't leave it on a shared drive.
      </p>
      {saveNote && (
        <p style={{ fontSize: 12, color: "var(--text-muted, #888)", marginTop: 4 }}>{saveNote}</p>
      )}
      <button className="connect-button" onClick={onUnlocked}>
        I've saved it - continue
      </button>
    </>,
  );
}
