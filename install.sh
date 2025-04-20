#!/bin/bash

PREFIX=${1:-"/opt/anylinuxfs"}

# sudo mkdir -p "$PREFIX"
mkdir -p "$PREFIX/bin"
mkdir -p "$PREFIX/libexec"

# sudo chown $(whoami):admin "$PREFIX"
cp bin/* "$PREFIX/bin/"

function fix_libkrun_path() {
    install_name_tool -change \
        /usr/local/lib/libkrun.dylib \
        "$PREFIX/lib/libkrun.dylib" "$1" 2>/dev/null
}

# not needed when using libkrun from homebrew
# fix_libkrun_path "$PREFIX/bin/anylinuxfs"
codesign --entitlements "anylinuxfs.entitlements" --force -s - "$PREFIX/bin/anylinuxfs"

cp -RL libexec/* "$PREFIX/libexec/"

# not needed when using libkrun from homebrew
# fix_libkrun_path "$PREFIX/libexec/init-rootfs"
codesign --entitlements "anylinuxfs.entitlements" --force -s - "$PREFIX/libexec/init-rootfs"
