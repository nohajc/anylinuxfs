#!/bin/bash

PREFIX="/opt/anylinuxfs"

sudo mkdir -p "$PREFIX"
mkdir -p "$PREFIX/bin"
mkdir -p "$PREFIX/libexec"

sudo chown $(whoami):admin "$PREFIX"
cp bin/* "$PREFIX/bin/"

function fix_libkrun_path() {
    install_name_tool -change \
        /usr/local/lib/libkrun.dylib \
        "$PREFIX/lib/libkrun.dylib" "$1" 2>/dev/null
}

fix_libkrun_path "$PREFIX/bin/anylinuxfs"
codesign --entitlements "anylinuxfs.entitlements" --force -s - "$PREFIX/bin/anylinuxfs"

fix_libkrun_path "$PREFIX/bin/init-rootfs"
codesign --entitlements "anylinuxfs.entitlements" --force -s - "$PREFIX/bin/init-rootfs"

cp -RL libexec/* "$PREFIX/libexec/"
