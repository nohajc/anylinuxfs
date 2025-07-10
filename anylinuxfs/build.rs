fn main() {
    // println!("cargo:rustc-link-search=/opt/homebrew/opt/libkrun/lib");
    println!("cargo:rustc-link-search=/Users/nohajan/gitprojs/3rd-party/libkrun/target/release");
    // println!("cargo:rustc-link-lib=static=krun");
    println!("cargo:rustc-link-lib=framework=Hypervisor");
}
