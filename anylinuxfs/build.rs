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
            // libkrun via pkg-config (pkg-config emits -L<libdir>).
            if let Ok(out) = std::process::Command::new("pkg-config")
                .args(["--libs-only-L", "libkrun"])
                .output()
                && out.status.success()
            {
                let path = String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .trim_start_matches("-L")
                    .to_string();
                if !path.is_empty() {
                    println!("cargo:rustc-link-search={}", path);
                }
            }
            println!("cargo:rustc-link-lib=krun");

            // libtirpc provides the SunRPC / rpcbind client API on Linux
            // (rpcb_set / rpcb_unset / rpcb_getmaps / getnetconfigent).
            // libtirpc is installed to a standard path, so linker search
            // dirs don't need tweaking.
            println!("cargo:rustc-link-lib=tirpc");
        }
        _ => {}
    }
}
