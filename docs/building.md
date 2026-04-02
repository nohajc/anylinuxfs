# Build from source

## Build dependencies

You need Go and Rust toolchains (not necessarily installed via Homebrew) and a few additional tools for cross-compiling the Linux helper running in the VM:

```sh
brew install go rustup
brew install lld llvm pkgconf
```

If you just installed `rustup` from Homebrew, initialise it first:

```sh
rustup default stable
export PATH="$PATH:/opt/homebrew/opt/rustup/bin"
```

## Runtime dependencies

You should already have these if you installed `anylinuxfs` from Homebrew (`libblkid` and the hypervisor):

```sh
brew install util-linux slp/krun/libkrun
```

## Building

```sh
git clone https://github.com/nohajc/anylinuxfs.git
cd anylinuxfs
rustup target add aarch64-unknown-linux-musl
rustup +nightly-2026-01-25 component add rust-src
./download-dependencies.sh
./build-app.sh             # debug build
./build-app.sh --release   # release build
```

The compiled executable will be available under `./bin`:

```sh
bin/anylinuxfs list
```
