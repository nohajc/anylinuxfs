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
        "linux" => {
            // Try pkg-config first; fall back to default lib search paths.
            if std::process::Command::new("pkg-config")
                .args(["--libs-only-L", "libkrun"])
                .output()
                .map(|o| {
                    if o.status.success() {
                        let path = String::from_utf8_lossy(&o.stdout)
                            .trim()
                            .trim_start_matches("-L")
                            .to_string();
                        if !path.is_empty() {
                            println!("cargo:rustc-link-search={}", path);
                        }
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false)
            {}
            println!("cargo:rustc-link-lib=krun");
        }
        _ => {}
    }
}
