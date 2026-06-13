//! H.264 software encoder wrapping the `openh264` crate. Turns RGBA
//! `VideoFrame`s (from the DisplayBackend) into Annex-B H.264 frames for the
//! screenshare pipeline. Decode happens in the webview (WebCodecs); the
//! openh264 decoder is used here only in tests to prove the output is valid.

use crate::display::VideoFrame;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod};
use openh264::OpenH264API;
use openh264::formats::{RgbaSliceU8, YUVBuffer};

/// One encoded H.264 frame.
pub struct EncodedFrame {
    /// Annex-B NAL byte stream (start codes; SPS/PPS inline before each IDR).
    pub data: Vec<u8>,
    /// True for an IDR/I keyframe — the frontend tags the WebCodecs chunk
    /// `'key'` vs `'delta'` from this.
    pub is_keyframe: bool,
    pub timestamp_ms: u64,
}

pub struct H264Encoder {
    enc: Encoder,
}

impl H264Encoder {
    pub fn new() -> Result<Self, String> {
        // 3 Mbps, 30 fps, keyframe every ~60 frames (~2 s). These are starting
        // values; Phase C/quality tuning revisits them (and NVENC in phase 2).
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(3_000_000))
            .max_frame_rate(FrameRate::from_hz(30.0))
            .intra_frame_period(IntraFramePeriod::from_num_frames(60));
        let enc = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| format!("openh264 encoder init: {e}"))?;
        Ok(Self { enc })
    }

    /// Force the NEXT encoded frame to be an IDR keyframe (call once before the
    /// first frame so a fresh decoder can start; later phases call it again
    /// when a new viewer attaches).
    pub fn force_keyframe(&mut self) {
        self.enc.force_intra_frame();
    }

    /// Encode one RGBA frame. The encoder derives width/height from the frame.
    pub fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedFrame, String> {
        let w = frame.width as usize;
        let h = frame.height as usize;
        // RgbaSliceU8 expects packed rows (stride == width*4). VideoFrame's
        // contract is packed RGBA8888; assert it so a padded frame fails loudly
        // rather than producing garbage.
        if frame.stride != w * 4 || frame.pixels.len() < h * w * 4 {
            return Err(format!(
                "frame not packed RGBA: stride={} expected={} pixels={} expected>={}",
                frame.stride, w * 4, frame.pixels.len(), h * w * 4
            ));
        }
        let src = RgbaSliceU8::new(&frame.pixels[..h * w * 4], (w, h));
        let yuv = YUVBuffer::from_rgb_source(src);
        let bitstream = self.enc.encode(&yuv).map_err(|e| format!("openh264 encode: {e}"))?;
        let is_keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        Ok(EncodedFrame {
            data: bitstream.to_vec(),
            is_keyframe,
            timestamp_ms: frame.timestamp_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packed RGBA test frame with a simple gradient (gives the encoder real
    /// content, not a flat color that could degenerate).
    fn gradient_frame(w: u32, h: u32, t: u64) -> VideoFrame {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                pixels[i] = (x.wrapping_add(t as u32) % 256) as u8;
                pixels[i + 1] = (y % 256) as u8;
                pixels[i + 2] = ((x + y) % 256) as u8;
                pixels[i + 3] = 255;
            }
        }
        VideoFrame { width: w, height: h, stride: (w * 4) as usize, pixels, timestamp_ms: t }
    }

    #[test]
    fn first_forced_frame_is_keyframe_and_nonempty() {
        let mut enc = H264Encoder::new().unwrap();
        enc.force_keyframe();
        let out = enc.encode(&gradient_frame(320, 240, 0)).unwrap();
        assert!(out.is_keyframe, "first forced frame must be a keyframe");
        assert!(!out.data.is_empty(), "encoded data must be non-empty");
        // Annex-B start code at the front.
        assert_eq!(&out.data[..4], &[0x00, 0x00, 0x00, 0x01], "expected Annex-B start code");
    }

    #[test]
    fn rejects_unpacked_frame() {
        let mut enc = H264Encoder::new().unwrap();
        let mut f = gradient_frame(16, 16, 0);
        f.stride = 999; // not width*4
        assert!(enc.encode(&f).is_err());
    }

    #[test]
    fn encoder_output_decodes_back_with_openh264() {
        // Proves the encoder emits valid H.264 a real decoder can consume.
        use openh264::decoder::Decoder;
        use openh264::formats::YUVSource;
        use openh264::nal_units;

        let mut enc = H264Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        enc.force_keyframe();

        let mut decoded_any = false;
        for t in 0..5u64 {
            let out = enc.encode(&gradient_frame(320, 240, t)).unwrap();
            for nal in nal_units(&out.data) {
                if let Ok(Some(yuv)) = dec.decode(nal) {
                    let (dw, dh) = yuv.dimensions();
                    assert_eq!((dw, dh), (320, 240), "decoded dims must match");
                    decoded_any = true;
                }
            }
        }
        assert!(decoded_any, "openh264 decoder must decode at least one frame from the encoder output");
    }
}
