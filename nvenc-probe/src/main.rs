//! THROWAWAY probe — validate NVENC hardware H.264 encoding before integrating
//! it into the screenshare encoder. The software path (openh264) already works;
//! NVENC is the perf upgrade for smooth 1080p60+ at low CPU. This can't be
//! compiled or run on the Linux/WSL side (no NVIDIA GPU), so it's validated on
//! the owner's Windows box first — same as the WASAPI and window-capture probes.
//!
//! It checks the make-or-break unknowns: the crate + cudarc BUILD on this
//! toolchain/driver, CUDA initialises, an NVENC encoder comes up on the GPU, and
//! it reports the supported codecs + input formats (which tell us whether we
//! feed NVENC NV12 or can hand it RGBA/ARGB directly). It does NOT encode a full
//! frame — that's exercised during the real integration.

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows + NVIDIA GPU only. Run this on the Windows box.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    use cudarc::driver::CudaContext;
    use nvidia_video_codec_sdk::safe::Encoder;

    // 1. CUDA: loads the driver libs at runtime (dynamic-loading) and binds GPU 0.
    let ctx = match CudaContext::new(0) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("CUDA init failed (NVIDIA driver / GPU present?): {e:?}");
            std::process::exit(1);
        }
    };
    println!("CUDA context created on GPU 0.");

    // 2. NVENC: bring up the hardware encoder on that CUDA context.
    let encoder = match Encoder::initialize_with_cuda(ctx) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("NVENC encoder init failed: {e:?}");
            std::process::exit(1);
        }
    };
    println!("NVENC encoder initialised.");

    // 3. Capabilities: which codecs + input formats does this GPU's NVENC offer?
    match encoder.get_encode_guids() {
        Ok(guids) => {
            println!("NVENC reports {} codec GUID(s).", guids.len());
            for (i, g) in guids.iter().enumerate() {
                match encoder.get_supported_input_formats(*g) {
                    Ok(fmts) => println!("  codec[{i}] input formats ({}): {:?}", fmts.len(), fmts),
                    Err(e) => println!("  codec[{i}] input-format query failed: {e:?}"),
                }
            }
        }
        Err(e) => {
            eprintln!("get_encode_guids failed: {e:?}");
            std::process::exit(1);
        }
    }

    println!("\nPROBE OK: nvidia-video-codec-sdk builds, CUDA + NVENC initialise on this machine.");
    println!("Paste this whole output back (especially the input-format list).");
}
