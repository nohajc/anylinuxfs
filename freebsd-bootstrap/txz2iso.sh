#!/bin/sh

mkdir tmp
trap 'rm -rf tmp' EXIT

tar xf $1 -C tmp
tar cf $2 --format iso9660 --strip-components=1 tmp/
#mkisofs -R -o $2 tmp/
