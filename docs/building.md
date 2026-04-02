# Build from Source

```
# build dependencies
brew install go rustup   # Skip if not relevant. You need Go and Rust toolchains but not necessarily installed via homebrew
brew install lld llvm pkgconf # These are for cross-compiling the Linux helper running in the VM

# if you just installed rustup from homebrew
rustup default stable
export PATH="$PATH:/opt/homebrew/opt/rustup/bin"

# runtime dependencies
brew install util-linux slp/krun/libkrun # Libblkid library and the hypervisor - you should already have these if you installed anylinuxfs from homebrew

# building anylinuxfs
git clone https://github.com/nohajc/anylinuxfs.git
cd anylinuxfs
rustup target add aarch64-unknown-linux-musl
rustup +nightly-2026-01-25 component add rust-src
./download-dependencies.sh
./build-app.sh             # debug build
./build-app.sh --release   # release build

# compiled executable will be available under ./bin
bin/anylinuxfs list
```
