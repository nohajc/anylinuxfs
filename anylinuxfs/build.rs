fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" => {
            println!(
                "cargo:rustc-link-search=framework={}",
                "/System/Library/PrivateFrameworks"
            );
            // CC_LINUX and PKG_CONFIG_PATH are set via .cargo/config.toml [env].
            // Re-run if someone forces them from the shell so Cargo picks up changes.
            println!("cargo:rerun-if-env-changed=CC_LINUX");
            println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
        }
        _ => {}
    }
}
