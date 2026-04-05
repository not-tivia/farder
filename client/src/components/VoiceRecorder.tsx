import { useState, useRef, useEffect } from "react";
import * as api from "../lib/tauri-bridge";

interface Props {
    onRecorded: (filePath: string, duration: number) => void;
    onCancel: () => void;
}

export default function VoiceRecorder({ onRecorded, onCancel }: Props) {
    const [recording, setRecording] = useState(true);
    const [duration, setDuration] = useState(0);
    const [error, setError] = useState<string | null>(null);
    const [filePath, setFilePath] = useState<string | null>(null);
    const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

    useEffect(() => {
        beginRecording();
        return () => {
            if (timerRef.current) clearInterval(timerRef.current);
        };
    }, []);

    async function beginRecording() {
        try {
            await api.startRecording();
            timerRef.current = setInterval(() => {
                setDuration(prev => prev + 1);
            }, 1000);
        } catch (e) {
            setError(String(e));
            setRecording(false);
        }
    }

    async function handleStop() {
        if (timerRef.current) clearInterval(timerRef.current);
        try {
            const path = await api.stopRecording();
            setFilePath(path);
            setRecording(false);
        } catch (e) {
            setError(String(e));
            setRecording(false);
        }
    }

    function handleSend() {
        if (filePath) {
            onRecorded(filePath, duration);
        }
    }

    function handleCancel() {
        if (timerRef.current) clearInterval(timerRef.current);
        // Try to stop if still recording
        api.stopRecording().catch(() => {});
        onCancel();
    }

    function formatDuration(secs: number): string {
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${s.toString().padStart(2, "0")}`;
    }

    if (error) {
        return (
            <div className="voice-recorder">
                <span style={{ color: "#cc0000", flex: 1 }}>Recording failed: {error}</span>
                <button className="xp-button" onClick={onCancel}>Close</button>
            </div>
        );
    }

    return (
        <div className="voice-recorder">
            {recording ? (
                <>
                    <span className="voice-recording-dot" />
                    <span className="voice-timer">{formatDuration(duration)}</span>
                    <button className="xp-button" onClick={handleStop}>Stop</button>
                    <button className="xp-button" onClick={handleCancel}>Cancel</button>
                </>
            ) : (
                <>
                    <span className="voice-timer">{formatDuration(duration)}</span>
                    <button className="xp-button" onClick={handleSend}>Send</button>
                    <button className="xp-button" onClick={handleCancel}>Discard</button>
                </>
            )}
        </div>
    );
}
