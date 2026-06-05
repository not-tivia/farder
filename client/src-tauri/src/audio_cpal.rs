// client/src-tauri/src/audio_cpal.rs
//
// Real AudioBackend backed by cpal. Bridges cpal's callback-based audio
// API into the AudioBackend trait's `mpsc` channel model.
//
// cpal `Stream`s (and the `Host`) are thread-affine (!Send): a stream must be
// created AND dropped on the same thread. Under tokio's multi-threaded runtime
// that is impossible to guarantee for a stream stored in a shared struct, which
// previously caused a "Dropped SendWrapper from a different thread" panic on
// disconnect. So all cpal objects live on a single dedicated audio thread; the
// backend is just a command channel to that thread.
//
// See docs/superpowers/specs/2026-05-25-cpal-audio-backend-design.md.

use crate::audio::{
    AudioBackend, AudioFormat, AudioInputDevice, AudioOutputDevice, PcmChunk,
};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Sample;
use std::sync::mpsc;
use std::time::Instant;

pub struct CpalAudioBackend {
    cmd_tx: mpsc::Sender<AudioCommand>,
}

/// Work sent to the dedicated audio thread. Each command carries a reply
/// channel so the (synchronous) trait methods can block for the result.
enum AudioCommand {
    EnumInputs(mpsc::Sender<Result<Vec<AudioInputDevice>, String>>),
    EnumOutputs(mpsc::Sender<Result<Vec<AudioOutputDevice>, String>>),
    StartCapture {
        device_id: Option<String>,
        format: AudioFormat,
        reply: mpsc::Sender<Result<mpsc::Receiver<PcmChunk>, String>>,
    },
    StopCapture(mpsc::Sender<Result<(), String>>),
    StartPlayback {
        device_id: Option<String>,
        format: AudioFormat,
        reply: mpsc::Sender<Result<mpsc::SyncSender<PcmChunk>, String>>,
    },
    StopPlayback(mpsc::Sender<Result<(), String>>),
}

impl CpalAudioBackend {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("farder-audio".into())
            .spawn(move || audio_thread(cmd_rx))
            .expect("spawn audio thread");
        Self { cmd_tx }
    }

    /// Send a command to the audio thread and block for its reply. Returns the
    /// payload `T` (usually itself a `Result`) or an error if the thread is gone.
    fn request<T>(
        &self,
        make: impl FnOnce(mpsc::Sender<T>) -> AudioCommand,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(make(reply_tx))
            .map_err(|_| "audio thread is not running".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "audio thread dropped the reply".to_string())
    }
}

impl Default for CpalAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// The dedicated audio thread. Owns the cpal host and the live streams, so
/// every stream is created and dropped here, never across threads. Exits when
/// the backend (and thus the command sender) is dropped.
fn audio_thread(rx: mpsc::Receiver<AudioCommand>) {
    let host = cpal::default_host();
    let mut capture: Option<cpal::Stream> = None;
    let mut playback: Option<cpal::Stream> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            AudioCommand::EnumInputs(reply) => {
                let _ = reply.send(enumerate_inputs(&host));
            }
            AudioCommand::EnumOutputs(reply) => {
                let _ = reply.send(enumerate_outputs(&host));
            }
            AudioCommand::StartCapture { device_id, format, reply } => {
                if capture.is_some() {
                    let _ = reply.send(Err("capture already active".into()));
                    continue;
                }
                match build_capture_stream(&host, device_id.as_deref(), format) {
                    Ok((stream, data_rx)) => {
                        capture = Some(stream);
                        let _ = reply.send(Ok(data_rx));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            AudioCommand::StopCapture(reply) => {
                capture = None; // dropped here, on the owning thread
                let _ = reply.send(Ok(()));
            }
            AudioCommand::StartPlayback { device_id, format, reply } => {
                if playback.is_some() {
                    let _ = reply.send(Err("playback already active".into()));
                    continue;
                }
                match build_playback_stream(&host, device_id.as_deref(), format) {
                    Ok((stream, data_tx)) => {
                        playback = Some(stream);
                        let _ = reply.send(Ok(data_tx));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            AudioCommand::StopPlayback(reply) => {
                playback = None;
                let _ = reply.send(Ok(()));
            }
        }
    }
    // Sender dropped: `capture` / `playback` drop here as the thread unwinds.
}

fn enumerate_inputs(host: &cpal::Host) -> Result<Vec<AudioInputDevice>, String> {
    let devices = host.input_devices().map_err(|e| format!("input_devices: {e}"))?;
    let default = host.default_input_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for (i, dev) in devices.enumerate() {
        let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
        let is_default = default.as_deref() == Some(name.as_str());
        out.push(AudioInputDevice { id: name.clone(), name, is_default });
    }
    Ok(out)
}

fn enumerate_outputs(host: &cpal::Host) -> Result<Vec<AudioOutputDevice>, String> {
    let devices = host.output_devices().map_err(|e| format!("output_devices: {e}"))?;
    let default = host.default_output_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for (i, dev) in devices.enumerate() {
        let name = dev.name().unwrap_or_else(|_| format!("device-{i}"));
        let is_default = default.as_deref() == Some(name.as_str());
        out.push(AudioOutputDevice { id: name.clone(), name, is_default });
    }
    Ok(out)
}

/// Average all interleaved channels of one device frame down to a single mono
/// sample. Empty frame -> silence.
fn downmix_frame_to_mono(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    frame.iter().sum::<f32>() / frame.len() as f32
}

/// Write one mono sample across `channels` interleaved output slots,
/// replicating it to every channel. Always writes at least one slot.
fn upmix_mono_into(sample: f32, channels: usize, out: &mut Vec<f32>) {
    for _ in 0..channels.max(1) {
        out.push(sample);
    }
}

/// Stateful linear-interpolation resampler for a single mono stream. Converts
/// `in_rate` -> `out_rate`, keeping phase continuity across `process` calls so
/// chunk boundaries don't click. Adequate for voice; not studio-grade.
struct LinearResampler {
    /// Input samples consumed per output sample (in_rate / out_rate).
    step: f64,
    /// Read position relative to `prev` (virtual index 0). Always >= 0.
    pos: f64,
    /// The most recent input sample, kept as the boundary sample (virtual
    /// index 0) so interpolation is continuous across calls.
    prev: f32,
    primed: bool,
}

impl LinearResampler {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        let step = if out_rate == 0 {
            1.0
        } else {
            in_rate as f64 / out_rate as f64
        };
        Self { step, pos: 0.0, prev: 0.0, primed: false }
    }

    /// Resample `input` (mono), appending output samples to `out`.
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        // On the first ever sample, seed `prev` from input[0] (no phantom
        // sample) and start interpolating from input[1] onward.
        let input = if !self.primed {
            self.primed = true;
            self.prev = input[0];
            &input[1..]
        } else {
            input
        };
        let n = input.len();
        if n == 0 {
            return;
        }
        // Virtual buffer v: v[0] = prev, v[k] = input[k-1] for k in 1..=n.
        let prev = self.prev;
        let step = self.step;
        let mut pos = self.pos;
        while (pos.floor() as usize) + 1 <= n {
            let lo = pos.floor() as usize;
            let frac = (pos - lo as f64) as f32;
            let a = if lo == 0 { prev } else { input[lo - 1] };
            let b = input[lo]; // v[lo+1] == input[lo] since lo+1 >= 1
            out.push(a + (b - a) * frac);
            pos += step;
        }
        // Re-base: the new origin (virtual index 0) becomes the last input
        // sample. `pos >= n` at loop exit, so the carried position stays >= 0.
        self.prev = input[n - 1];
        self.pos = pos - n as f64;
    }
}

/// Rank of a device sample format we can build a stream for (lower = better).
/// `None` means we don't handle it. F32 needs no conversion so it wins.
fn supported_sample_format_rank(f: cpal::SampleFormat) -> Option<u8> {
    match f {
        cpal::SampleFormat::F32 => Some(0),
        cpal::SampleFormat::I16 => Some(1),
        cpal::SampleFormat::I32 => Some(2),
        cpal::SampleFormat::U16 => Some(3),
        _ => None,
    }
}

/// Choose a `StreamConfig` + sample format for the device. Prefers
/// `want_channels` then stereo then any channel count (the callbacks down/upmix
/// between mono and whatever the device offers); prefers a config whose range
/// covers `want_sr` so no resampling is needed, else falls back to the device's
/// native rate; and prefers F32 then I16/I32/U16 (the callbacks convert to/from
/// f32). On failure the error lists what the device exposes.
fn choose_stream_config(
    configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    want_sr: cpal::SampleRate,
    want_channels: u16,
    label: &str,
    format: &AudioFormat,
) -> Result<(cpal::StreamConfig, cpal::SampleFormat), String> {
    let mut usable: Vec<cpal::SupportedStreamConfigRange> = Vec::new();
    let mut available: Vec<String> = Vec::new();
    for cfg in configs {
        available.push(format!(
            "{:?}/{}ch/{}-{}Hz",
            cfg.sample_format(),
            cfg.channels(),
            cfg.min_sample_rate().0,
            cfg.max_sample_rate().0
        ));
        if supported_sample_format_rank(cfg.sample_format()).is_some() {
            usable.push(cfg);
        }
    }
    // Lower is better: exact channel match, then stereo, then anything; ties
    // broken by preferred sample format.
    let chan_rank = |ch: u16| -> u8 {
        if ch == want_channels {
            0
        } else if ch == 2 {
            1
        } else {
            2
        }
    };
    let fmt_rank = |c: &cpal::SupportedStreamConfigRange| {
        supported_sample_format_rank(c.sample_format()).unwrap_or(u8::MAX)
    };
    // Pass 1: a config whose range already covers want_sr (no resampling).
    let covering = usable
        .iter()
        .filter(|c| c.min_sample_rate() <= want_sr && want_sr <= c.max_sample_rate())
        .min_by_key(|c| (chan_rank(c.channels()), fmt_rank(c)));
    if let Some(c) = covering {
        return Ok((c.clone().with_sample_rate(want_sr).config(), c.sample_format()));
    }
    // Pass 2: device can't do want_sr; best channel+format at its lowest native
    // rate, and the callback resamples to/from it.
    let fallback = usable
        .iter()
        .min_by_key(|c| (chan_rank(c.channels()), fmt_rank(c), c.min_sample_rate().0));
    if let Some(c) = fallback {
        let rate = c.min_sample_rate();
        return Ok((c.clone().with_sample_rate(rate).config(), c.sample_format()));
    }
    Err(format!(
        "no supported {label} config for {format:?}; device offers: [{}]",
        available.join(", ")
    ))
}

/// Build (and start) an input stream of device sample type `T`, converting each
/// device sample to f32 before handing interleaved frames to `process`.
fn run_input_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut process: impl FnMut(&[f32]) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device
        .build_input_stream(
            config,
            move |raw: &[T], _: &cpal::InputCallbackInfo| {
                scratch.clear();
                scratch.extend(raw.iter().map(|&s| f32::from_sample(s)));
                process(&scratch);
            },
            |err| eprintln!("[audio] cpal capture error: {err}"),
            None,
        )
        .map_err(|e| format!("build_input_stream: {e}"))?;
    use cpal::traits::StreamTrait;
    stream.play().map_err(|e| format!("stream.play: {e}"))?;
    Ok(stream)
}

/// Build (and start) an output stream of device sample type `T`. `fill` writes
/// f32 output; each sample is converted to `T` for the device.
fn run_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut fill: impl FnMut(&mut [f32]) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static,
{
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                scratch.clear();
                scratch.resize(out.len(), 0.0);
                fill(&mut scratch);
                for (o, &s) in out.iter_mut().zip(scratch.iter()) {
                    *o = T::from_sample(s);
                }
            },
            |err| eprintln!("[audio] cpal playback error: {err}"),
            None,
        )
        .map_err(|e| format!("build_output_stream: {e}"))?;
    use cpal::traits::StreamTrait;
    stream.play().map_err(|e| format!("stream.play: {e}"))?;
    Ok(stream)
}

/// Pick an input device by name, or the host's default if `device_id` is None.
fn pick_input_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    match device_id {
        None => host.default_input_device()
            .ok_or_else(|| "no default input device".to_string()),
        Some(name) => host.input_devices()
            .map_err(|e| format!("input_devices: {e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("input device not found: {name}")),
    }
}

fn build_input_config(
    device: &cpal::Device,
    format: &AudioFormat,
) -> Result<(cpal::StreamConfig, cpal::SampleFormat), String> {
    let want_sr = cpal::SampleRate(format.sample_rate);
    let configs = device.supported_input_configs()
        .map_err(|e| format!("supported_input_configs: {e}"))?;
    choose_stream_config(configs, want_sr, format.channels, "input", format)
}

fn pick_output_device(host: &cpal::Host, device_id: Option<&str>) -> Result<cpal::Device, String> {
    match device_id {
        None => host.default_output_device()
            .ok_or_else(|| "no default output device".to_string()),
        Some(name) => host.output_devices()
            .map_err(|e| format!("output_devices: {e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("output device not found: {name}")),
    }
}

fn build_output_config(
    device: &cpal::Device,
    format: &AudioFormat,
) -> Result<(cpal::StreamConfig, cpal::SampleFormat), String> {
    let want_sr = cpal::SampleRate(format.sample_rate);
    let configs = device.supported_output_configs()
        .map_err(|e| format!("supported_output_configs: {e}"))?;
    choose_stream_config(configs, want_sr, format.channels, "output", format)
}

/// Build (and start) a capture stream on the audio thread, returning it
/// alongside the channel that delivers mono `PcmChunk`s at the engine rate.
fn build_capture_stream(
    host: &cpal::Host,
    device_id: Option<&str>,
    format: AudioFormat,
) -> Result<(cpal::Stream, mpsc::Receiver<PcmChunk>), String> {
    if format.channels == 0 || format.samples_per_chunk == 0 {
        return Err(format!("invalid AudioFormat: {format:?}"));
    }
    let device = pick_input_device(host, device_id)?;
    let (cpal_config, sample_fmt) = build_input_config(&device, &format)?;
    let dev_channels = cpal_config.channels as usize;
    let want_channels = format.channels as usize;
    let samples_per_chunk = format.samples_per_chunk;
    let device_rate = cpal_config.sample_rate.0;
    let engine_rate = format.sample_rate;
    let (tx, rx) = mpsc::sync_channel::<PcmChunk>(8);

    let started = Instant::now();
    let mut buffered: Vec<f32> = Vec::with_capacity(samples_per_chunk);
    // Resample the device's native rate to the engine rate (48k) if they
    // differ (e.g. a 96 kHz-only device).
    let mut resampler = if device_rate != engine_rate {
        Some(LinearResampler::new(device_rate, engine_rate))
    } else {
        None
    };
    let mut mono_scratch: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();

    // Process interleaved f32 device samples (the typed stream converts to f32
    // first): downmix each frame to mono, resample to the engine rate, and
    // accumulate into fixed-size chunks.
    let process = move |raw: &[f32]| {
        mono_scratch.clear();
        let mut i = 0;
        while i + dev_channels <= raw.len() {
            let sample = if dev_channels == want_channels {
                raw[i]
            } else {
                downmix_frame_to_mono(&raw[i..i + dev_channels])
            };
            mono_scratch.push(sample);
            i += dev_channels;
        }
        let engine_samples: &[f32] = match resampler.as_mut() {
            Some(r) => {
                resampled.clear();
                r.process(&mono_scratch, &mut resampled);
                &resampled
            }
            None => &mono_scratch,
        };
        for &s in engine_samples {
            buffered.push(s);
            if buffered.len() >= samples_per_chunk {
                let chunk = PcmChunk {
                    samples: std::mem::take(&mut buffered),
                    timestamp_ms: started.elapsed().as_millis() as u64,
                };
                buffered.reserve(samples_per_chunk);
                let _ = tx.try_send(chunk);
            }
        }
    };

    let stream = match sample_fmt {
        cpal::SampleFormat::F32 => run_input_stream::<f32>(&device, &cpal_config, process)?,
        cpal::SampleFormat::I16 => run_input_stream::<i16>(&device, &cpal_config, process)?,
        cpal::SampleFormat::I32 => run_input_stream::<i32>(&device, &cpal_config, process)?,
        cpal::SampleFormat::U16 => run_input_stream::<u16>(&device, &cpal_config, process)?,
        other => return Err(format!("unsupported input sample format: {other:?}")),
    };
    Ok((stream, rx))
}

/// Build (and start) a playback stream on the audio thread, returning it
/// alongside the channel that accepts mono `PcmChunk`s at the engine rate.
fn build_playback_stream(
    host: &cpal::Host,
    device_id: Option<&str>,
    format: AudioFormat,
) -> Result<(cpal::Stream, mpsc::SyncSender<PcmChunk>), String> {
    if format.channels == 0 || format.samples_per_chunk == 0 {
        return Err(format!("invalid AudioFormat: {format:?}"));
    }
    let device = pick_output_device(host, device_id)?;
    let (cpal_config, sample_fmt) = build_output_config(&device, &format)?;
    let dev_channels = cpal_config.channels as usize;
    let want_channels = format.channels as usize;

    // Buffer sized for ~500ms of audio (matches the mock's behavior).
    let frames_per_chunk = (format.samples_per_chunk / want_channels).max(1);
    let chunks_per_500ms = ((format.sample_rate as f32 * 0.5)
        / frames_per_chunk as f32).ceil() as usize;
    let buf = chunks_per_500ms.max(2);
    let (tx, rx) = mpsc::sync_channel::<PcmChunk>(buf);

    let device_rate = cpal_config.sample_rate.0;
    let engine_rate = format.sample_rate;
    let mut pending: Vec<f32> = Vec::new();
    // Resample the engine rate (48k) to the device's native rate if they
    // differ (e.g. a 96 kHz-only device).
    let mut resampler = if device_rate != engine_rate {
        Some(LinearResampler::new(engine_rate, device_rate))
    } else {
        None
    };
    let mut resampled: Vec<f32> = Vec::new();

    // Fill an f32 output buffer (the typed stream converts to the device
    // format): pull engine chunks, resample to device rate, upmix to the device
    // channel count; underrun -> silence.
    let fill = move |out: &mut [f32]| {
        while pending.len() < out.len() {
            match rx.try_recv() {
                Ok(chunk) => {
                    let mono: &[f32] = match resampler.as_mut() {
                        Some(r) => {
                            resampled.clear();
                            r.process(&chunk.samples, &mut resampled);
                            &resampled
                        }
                        None => &chunk.samples,
                    };
                    if dev_channels == want_channels {
                        pending.extend_from_slice(mono);
                    } else {
                        for &s in mono {
                            upmix_mono_into(s, dev_channels, &mut pending);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let take = pending.len().min(out.len());
        out[..take].copy_from_slice(&pending[..take]);
        pending.drain(..take);
        for s in &mut out[take..] {
            *s = 0.0;
        }
    };

    let stream = match sample_fmt {
        cpal::SampleFormat::F32 => run_output_stream::<f32>(&device, &cpal_config, fill)?,
        cpal::SampleFormat::I16 => run_output_stream::<i16>(&device, &cpal_config, fill)?,
        cpal::SampleFormat::I32 => run_output_stream::<i32>(&device, &cpal_config, fill)?,
        cpal::SampleFormat::U16 => run_output_stream::<u16>(&device, &cpal_config, fill)?,
        other => return Err(format!("unsupported output sample format: {other:?}")),
    };
    Ok((stream, tx))
}

impl AudioBackend for CpalAudioBackend {
    fn enumerate_input_devices(&self) -> Result<Vec<AudioInputDevice>, String> {
        self.request(AudioCommand::EnumInputs)?
    }

    fn enumerate_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        self.request(AudioCommand::EnumOutputs)?
    }

    fn start_capture(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::Receiver<PcmChunk>, String> {
        let device_id = device_id.map(|s| s.to_string());
        self.request(move |reply| AudioCommand::StartCapture { device_id, format, reply })?
    }

    fn stop_capture(&self) -> Result<(), String> {
        self.request(AudioCommand::StopCapture)?
    }

    fn start_playback(
        &self,
        device_id: Option<&str>,
        format: AudioFormat,
    ) -> Result<mpsc::SyncSender<PcmChunk>, String> {
        let device_id = device_id.map(|s| s.to_string());
        self.request(move |reply| AudioCommand::StartPlayback { device_id, format, reply })?
    }

    fn stop_playback(&self) -> Result<(), String> {
        self.request(AudioCommand::StopPlayback)?
    }

    fn backend_name(&self) -> &'static str {
        "cpal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_backend_constructs_without_panicking() {
        let _backend = CpalAudioBackend::new();
    }

    #[test]
    fn cpal_backend_name_is_cpal() {
        let backend = CpalAudioBackend::new();
        assert_eq!(backend.backend_name(), "cpal");
    }

    #[test]
    fn cpal_enumerate_input_devices_returns_vec() {
        let backend = CpalAudioBackend::new();
        let _devices = backend.enumerate_input_devices().expect("enumerate input");
    }

    #[test]
    fn cpal_enumerate_output_devices_returns_vec() {
        let backend = CpalAudioBackend::new();
        let _devices = backend.enumerate_output_devices().expect("enumerate output");
    }

    #[test]
    fn downmix_stereo_frame_averages_channels() {
        assert!((downmix_frame_to_mono(&[0.4, 0.6]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_surround_frame_averages_all_channels() {
        // 6-channel (5.1) frame averages to 0.5.
        let frame = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
        assert!((downmix_frame_to_mono(&frame) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_mono_frame_passes_through() {
        assert!((downmix_frame_to_mono(&[0.7]) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn downmix_empty_frame_is_silent() {
        assert_eq!(downmix_frame_to_mono(&[]), 0.0);
    }

    #[test]
    fn upmix_mono_to_stereo_duplicates_sample() {
        let mut out = Vec::new();
        upmix_mono_into(0.7, 2, &mut out);
        assert_eq!(out, vec![0.7, 0.7]);
    }

    #[test]
    fn upmix_mono_to_surround_replicates_to_all_channels() {
        let mut out = Vec::new();
        upmix_mono_into(0.3, 6, &mut out);
        assert_eq!(out, vec![0.3; 6]);
    }

    #[test]
    fn upmix_zero_channels_emits_at_least_one() {
        // Defensive: never silently drop a sample.
        let mut out = Vec::new();
        upmix_mono_into(0.9, 0, &mut out);
        assert_eq!(out, vec![0.9]);
    }

    #[test]
    fn resampler_identity_tracks_input() {
        let mut r = LinearResampler::new(48000, 48000);
        let mut out = Vec::new();
        r.process(&[0.0, 1.0, 2.0, 3.0, 4.0], &mut out);
        // 1-sample streaming delay; last sample is retained for continuity.
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn resampler_2x_upsample_interpolates() {
        let mut r = LinearResampler::new(48000, 96000);
        let mut out = Vec::new();
        r.process(&[0.0, 2.0, 4.0], &mut out);
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn resampler_2x_downsample_drops_alternate() {
        let mut r = LinearResampler::new(96000, 48000);
        let mut out = Vec::new();
        r.process(&[0.0, 1.0, 2.0, 3.0, 4.0], &mut out);
        assert_eq!(out, vec![0.0, 2.0]);
    }

    #[test]
    fn resampler_is_continuous_across_calls() {
        // Splitting the input must give the same output as one call.
        let input: Vec<f32> = (0..20).map(|i| i as f32).collect();

        let mut whole = LinearResampler::new(96000, 48000);
        let mut out_whole = Vec::new();
        whole.process(&input, &mut out_whole);

        let mut split = LinearResampler::new(96000, 48000);
        let mut out_split = Vec::new();
        split.process(&input[..7], &mut out_split);
        split.process(&input[7..], &mut out_split);

        assert_eq!(out_whole, out_split);
    }

    #[test]
    fn resampler_handles_single_sample_first_call() {
        // First call primes prev from input[0] and emits nothing; next call
        // continues without panicking.
        let mut r = LinearResampler::new(96000, 48000);
        let mut out = Vec::new();
        r.process(&[1.0], &mut out);
        assert!(out.is_empty());
        r.process(&[2.0, 3.0, 4.0], &mut out);
        // Stream so far is [1,2,3,4]; 2x downsample -> ~[1,3].
        assert_eq!(out, vec![1.0, 3.0]);
    }
}
