//! The universal volume control: a master volume and a loudness CEILING.
//!
//! The ceiling is the point. A normal volume slider scales everything equally, so
//! if someone screams into their microphone the scream is still louder than
//! everyone else by the same amount. A ceiling says "nothing may be louder than X
//! dB", and anything that would be is turned down until it is not. The master
//! volume is applied first, so raising it can never push sound past the ceiling.
//!
//! This sits at the very last step of the voice/screen-audio mixer, so it covers
//! everyone's voice and shared-screen audio together. (Videos and voice messages
//! in chat play in the web view and have their own matching stage in
//! `client/src/lib/audioLimiter.ts`.)
//!
//! The limiter attacks instantly (so the ceiling is a hard guarantee for each
//! sample) and recovers smoothly (so it does not pump). It makes no attempt to be
//! pretty: it exists to protect ears.

use std::sync::atomic::{AtomicU32, Ordering};

/// Master volume range: 0 = silent, 1 = unchanged, up to 150%.
pub const MASTER_MAX: f32 = 1.5;
/// The ceiling can be set between these (dB relative to full scale). Anything
/// at or above [`CEILING_OFF_ABOVE_DB`] means "no ceiling".
pub const CEILING_MIN_DB: f32 = -40.0;
pub const CEILING_MAX_DB: f32 = -3.0;
pub const CEILING_OFF_ABOVE_DB: f32 = -0.5;

// Stored as bits so the audio thread can read them without a lock.
static MASTER: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0
static CEILING_DB: AtomicU32 = AtomicU32::new(0); // 0.0 = off

/// A volume from outside (a settings file, a slider) made safe: not-a-number or
/// infinite becomes unchanged, out of range is pulled into range.
pub fn clamp_master(v: f32) -> f32 {
    if !v.is_finite() { 1.0 } else { v.clamp(0.0, MASTER_MAX) }
}

/// A ceiling from outside made safe. `0.0` means off; so does anything that is
/// not a number or is essentially full scale.
pub fn clamp_ceiling_db(v: f32) -> f32 {
    if !v.is_finite() || v >= CEILING_OFF_ABOVE_DB { 0.0 } else { v.clamp(CEILING_MIN_DB, CEILING_MAX_DB) }
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Set both values (clamped). Called when the settings change and at startup.
pub fn set(master: f32, ceiling_db: f32) {
    MASTER.store(clamp_master(master).to_bits(), Ordering::Release);
    CEILING_DB.store(clamp_ceiling_db(ceiling_db).to_bits(), Ordering::Release);
}

/// The current master volume and, if a ceiling is set, its linear level.
pub fn current() -> (f32, Option<f32>) {
    let master = f32::from_bits(MASTER.load(Ordering::Acquire));
    let db = f32::from_bits(CEILING_DB.load(Ordering::Acquire));
    (master, if db < 0.0 { Some(db_to_linear(db)) } else { None })
}

/// The settings as the UI sees them: (master 0..1.5, ceiling dB, 0 = off).
pub fn current_settings() -> (f32, f32) {
    (
        f32::from_bits(MASTER.load(Ordering::Acquire)),
        f32::from_bits(CEILING_DB.load(Ordering::Acquire)),
    )
}

/// The gain stage. Keep one per output stream; it carries its state across
/// frames so a loud sound that spans two frames is handled as one.
pub struct Limiter {
    gain: f32,
    release_coef: f32,
}

impl Limiter {
    pub fn new(sample_rate: u32, release_ms: f32) -> Self {
        let samples = (sample_rate as f32 * release_ms / 1000.0).max(1.0);
        Self { gain: 1.0, release_coef: 1.0 - (-1.0 / samples).exp() }
    }

    /// Apply `master`, then (if there is a ceiling) turn down whatever exceeds it.
    /// With a ceiling, no output sample is ever larger than `ceiling`.
    pub fn process(&mut self, buf: &mut [f32], master: f32, ceiling: Option<f32>) {
        let master = clamp_master(master);
        match ceiling {
            None => {
                self.gain = 1.0;
                for s in buf.iter_mut() {
                    // Never wrap or overflow the converter downstream.
                    *s = (*s * master).clamp(-1.0, 1.0);
                }
            }
            Some(ceil) => {
                // A hair under, so floating-point rounding can never poke through.
                let ceil = ceil * 0.9999;
                for s in buf.iter_mut() {
                    let x = *s * master;
                    let ax = x.abs();
                    // Recover smoothly toward unity, but never to more than THIS
                    // sample allows: that single `min` is both the instant attack
                    // (a loud sample pulls the gain straight down) and the
                    // guarantee (the gain used can never put it over the ceiling).
                    let released = self.gain + (1.0 - self.gain) * self.release_coef;
                    let allowed = if ax > 0.0 { ceil / ax } else { f32::INFINITY };
                    self.gain = released.min(allowed).min(1.0);
                    *s = x * self.gain;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(amp: f32, hz: f32, n: usize, start: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * ((start + i) as f32 / SR as f32)).sin())
            .collect()
    }

    /// Run `signal` through a limiter in 960-sample frames (as the mixer does).
    fn run(signal: &[f32], master: f32, ceiling: Option<f32>) -> Vec<f32> {
        let mut l = Limiter::new(SR, 150.0);
        let mut out = signal.to_vec();
        for frame in out.chunks_mut(960) {
            l.process(frame, master, ceiling);
        }
        out
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn db_conversion_is_right() {
        assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-6);
        assert!((db_to_linear(-6.0206) - 0.5).abs() < 1e-4);
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nothing_ever_exceeds_the_ceiling_whatever_is_thrown_at_it() {
        for db in [-40.0f32, -24.0, -12.0, -6.0, -3.0] {
            let ceil = db_to_linear(db);
            // A full-scale scream, boosted by the maximum master volume.
            let scream = sine(1.0, 440.0, 48_000, 0);
            assert!(peak(&run(&scream, MASTER_MAX, Some(ceil))) <= ceil, "sine at {db} dB");
            // Square wave: the harshest case, every sample at full scale.
            let square: Vec<f32> = (0..48_000).map(|i| if (i / 50) % 2 == 0 { 1.0 } else { -1.0 }).collect();
            assert!(peak(&run(&square, MASTER_MAX, Some(ceil))) <= ceil, "square at {db} dB");
            // Silence suddenly followed by a huge burst.
            let mut step = vec![0.0f32; 4_800];
            step.extend(std::iter::repeat(8.0f32).take(4_800));
            assert!(peak(&run(&step, 1.0, Some(ceil))) <= ceil, "step at {db} dB");
            // Pseudo-random noise far above full scale.
            let mut x: u32 = 12345;
            let noise: Vec<f32> = (0..48_000)
                .map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); ((x >> 8) as f32 / 8_388_608.0 - 1.0) * 6.0 })
                .collect();
            assert!(peak(&run(&noise, MASTER_MAX, Some(ceil))) <= ceil, "noise at {db} dB");
        }
    }

    #[test]
    fn the_ceiling_holds_across_frame_boundaries() {
        // A loud sound that starts in the last samples of one frame.
        let ceil = db_to_linear(-12.0);
        let mut sig = vec![0.0f32; 950];
        sig.extend(std::iter::repeat(1.0f32).take(2_000));
        assert!(peak(&run(&sig, 1.0, Some(ceil))) <= ceil);
    }

    #[test]
    fn quiet_sound_passes_through_untouched() {
        let quiet = sine(0.01, 300.0, 9_600, 0);
        let out = run(&quiet, 1.0, Some(db_to_linear(-20.0)));
        for (a, b) in quiet.iter().zip(&out) {
            assert!((a - b).abs() < 1e-6, "a sound below the ceiling must not change");
        }
    }

    #[test]
    fn the_master_volume_applies_before_the_ceiling_so_it_cannot_beat_it() {
        let ceil = db_to_linear(-12.0);
        // Normal talking level, volume turned all the way up.
        let talk = sine(0.2, 200.0, 9_600, 0);
        let out = run(&talk, MASTER_MAX, Some(ceil));
        assert!(peak(&out) <= ceil);
        // And turning the volume down does scale a sound that is under the ceiling.
        let half = run(&talk, 0.5, Some(ceil));
        assert!((peak(&half) - 0.1).abs() < 0.002, "0.2 at 50% is 0.1, got {}", peak(&half));
    }

    #[test]
    fn it_recovers_after_a_scream_instead_of_staying_quiet() {
        let ceil = db_to_linear(-12.0);
        let mut l = Limiter::new(SR, 150.0);
        let mut burst = vec![1.0f32; 960];
        l.process(&mut burst, 1.0, Some(ceil));
        // Then ordinary quiet speech for 1 second: the gain must climb back to ~unity.
        let quiet = sine(0.02, 250.0, 48_000, 0);
        let mut out = quiet.clone();
        for frame in out.chunks_mut(960) { l.process(frame, 1.0, Some(ceil)); }
        let tail = &out[out.len() - 960..];
        let src = &quiet[quiet.len() - 960..];
        let ratio = peak(tail) / peak(src);
        assert!(ratio > 0.99, "after a second the quiet voice should be back at full level, got {ratio}");
        // ...and not instantly (that would defeat smooth release): shortly after the burst it is still reduced.
        let mut l2 = Limiter::new(SR, 150.0);
        let mut burst2 = vec![1.0f32; 960];
        l2.process(&mut burst2, 1.0, Some(ceil));
        let mut soon = sine(0.02, 250.0, 480, 0);
        l2.process(&mut soon, 1.0, Some(ceil));
        assert!(peak(&soon) < 0.02 * 0.9, "right after the scream the voice is still being held down");
    }

    #[test]
    fn with_no_ceiling_only_the_master_applies_and_nothing_overflows() {
        let s = vec![0.5f32; 960];
        assert!((run(&s, 0.5, None)[0] - 0.25).abs() < 1e-6);
        // Boosted past full scale is held at full scale, never wrapped.
        let loud = vec![0.9f32; 960];
        assert!(run(&loud, MASTER_MAX, None).iter().all(|v| *v <= 1.0));
        // Silent master is silent.
        assert!(run(&loud, 0.0, Some(0.5)).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn settings_from_outside_are_made_safe() {
        assert_eq!(clamp_master(f32::NAN), 1.0);
        assert_eq!(clamp_master(f32::INFINITY), 1.0);
        assert_eq!(clamp_master(-3.0), 0.0);
        assert_eq!(clamp_master(99.0), MASTER_MAX);
        assert_eq!(clamp_master(0.8), 0.8);
        assert_eq!(clamp_ceiling_db(f32::NAN), 0.0, "garbage means OFF, not a dangerous guess");
        assert_eq!(clamp_ceiling_db(0.0), 0.0);
        assert_eq!(clamp_ceiling_db(5.0), 0.0);
        assert_eq!(clamp_ceiling_db(-0.2), 0.0, "essentially full scale is off");
        assert_eq!(clamp_ceiling_db(-1.0), CEILING_MAX_DB, "a ceiling above -3 dB is held at -3 dB");
        assert_eq!(clamp_ceiling_db(-90.0), CEILING_MIN_DB);
        assert_eq!(clamp_ceiling_db(-18.0), -18.0);
    }

    #[test]
    fn the_defaults_do_nothing() {
        // Fresh process state: unchanged volume, no ceiling.
        let (master, ceil) = current();
        assert_eq!(master, 1.0);
        assert!(ceil.is_none());
    }
}
