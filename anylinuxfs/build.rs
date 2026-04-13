fn main() {
    println!("cargo:rustc-link-search=/opt/homebrew/opt/libkrun/lib");
    println!(
        "cargo:rustc-link-search=framework={}",
        "/System/Library/PrivateFrameworks"
    );
    // CC_LINUX and PKG_CONFIG_PATH are set via .cargo/config.toml [env].
    // Re-run if someone forces them from the shell so Cargo picks up changes.
    println!("cargo:rerun-if-env-changed=CC_LINUX");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
}
