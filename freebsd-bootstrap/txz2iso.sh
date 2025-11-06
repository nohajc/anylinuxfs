#!/bin/sh

mkdir tmp
trap 'rm -rf tmp' EXIT

tar xf $1 -C tmp
mkisofs -R -o $2 tmp/
