fn main() {
    println!("cargo:rustc-link-search=/opt/homebrew/opt/libkrun/lib");
    println!(
        "cargo:rustc-link-search=framework={}",
        "/System/Library/PrivateFrameworks"
    );
}
