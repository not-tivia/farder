import { useEffect, useState, type ReactNode } from "react";
import * as api from "../lib/tauri-bridge";

type Screen = "loading" | "set-pin" | "enter-pin" | "migrate" | "restore" | "show-phrase";

// Reusable 4-digit PIN field (digits only, max length 4).
function PinField({
  value,
  onChange,
  autoFocus,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  autoFocus?: boolean;
  placeholder?: string;
}) {
  return (
    <input
      className="connect-input"
      type="password"
      inputMode="numeric"
      autoFocus={autoFocus}
      placeholder={placeholder ?? "4-digit PIN"}
      value={value}
      maxLength={4}
      onChange={(e) => onChange(e.target.value.replace(/\D/g, "").slice(0, 4))}
    />
  );
}

export default function IdentityGate({ onUnlocked }: { onUnlocked: () => void }) {
  const [screen, setScreen] = useState<Screen>("loading");
  const [pin, setPin] = useState("");
  const [pin2, setPin2] = useState("");
  const [phrase, setPhrase] = useState("");
  const [recoveryPhrase, setRecoveryPhrase] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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
        <PinField value={pin} onChange={setPin} autoFocus placeholder="Choose PIN" />
        <PinField value={pin2} onChange={setPin2} placeholder="Confirm PIN" />
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
        <PinField value={pin} onChange={setPin} autoFocus placeholder="Choose PIN" />
        <PinField value={pin2} onChange={setPin2} placeholder="Confirm PIN" />
        <button className="connect-button" disabled={busy} onClick={handleMigrate}>
          {busy ? "Securing..." : "Secure account"}
        </button>
      </>,
    );

  if (screen === "enter-pin")
    return shell(
      "Enter your PIN",
      <>
        <PinField value={pin} onChange={setPin} autoFocus />
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
        <PinField value={pin} onChange={setPin} placeholder="New PIN" />
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
      <button className="connect-button" onClick={onUnlocked}>
        I've saved it - continue
      </button>
    </>,
  );
}
