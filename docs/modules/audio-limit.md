# Universal volume control (master volume + loudness ceiling)

**What it is:** one place to say how loud Farder may ever be. Two sliders
(Settings -> Sound):

- **Volume** (0 to 150%): scales everything up or down.
- **Max loudness** (off, or -40 to -3 dB): a CEILING. Anything that would be
  louder is turned down until it is not, so a shout into a microphone or a loud
  clip cannot hurt, however high the volume is. Volume is applied FIRST, so
  raising it can never push sound past the ceiling.

## Where it applies

| Sound | Limited by | Notes |
|---|---|---|
| Voice calls, shared-screen audio | `audio_limit.rs`, the LAST step of `voice/mixer.rs` | Hard guarantee per sample; the echo canceller sees what actually plays |
| Videos and voice messages in chat | `lib/audioLimiter.ts` + `audioLimiterWorklet.ts` | An AudioWorklet running the SAME algorithm |
| YouTube / Spotify players (iframes) | not reachable | A page cannot process another origin's audio; they keep their own volume |

## The algorithm (identical in both places)
Ceiling as a linear level, `x = sample * master`, `gain = min(gain recovering
toward 1 over ~150 ms, ceiling / |x|, 1)`, `out = x * gain`. The single `min` is
both the instant attack and the guarantee: the gain used can never put a sample
over the ceiling (ceiling scaled by 0.9999 so rounding cannot poke through). With
no ceiling only the master applies, hard-held at full scale so nothing wraps.
Stereo is limited together so the balance does not wobble.

## Safe by default
At 100% / no ceiling the web-view code touches no element at all. Settings from
outside (a file, a slider) are clamped; garbage means unchanged / off.

## Tests
- Rust: `cargo test audio_limit` (9, incl. screams, square waves, noise, steps,
  frame boundaries, recovery) and `cargo test mixer` (the mixer itself holds a
  -12 dB ceiling on a full-scale peer at 3x gain; the same input unlimited does not).
- Web: `bash client/scripts/run-unit-tests.sh` runs the SHIPPED worklet source
  against the same properties.

Not verified until run on Windows: that sound actually comes out of speakers
through the worklet (autoplay-policy / context resume), and the sliders.
