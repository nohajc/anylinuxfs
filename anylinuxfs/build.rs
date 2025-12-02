fn main() {
    if !cfg!(feature = "freebsd") {
        println!("cargo:rustc-link-search=/opt/homebrew/opt/libkrun/lib");
    } else {
        println!("cargo:rustc-link-search=/usr/local/lib");
    }
    println!(
        "cargo:rustc-link-search=framework={}",
        "/System/Library/PrivateFrameworks"
    );
}
