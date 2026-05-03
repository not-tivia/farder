fn main() {
    // Pass the target triple so server_manager can find the sidecar binary
    println!("cargo:rustc-env=TARGET={}", std::env::var("TARGET").unwrap());
    tauri_build::build();
}
