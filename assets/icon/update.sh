#!/bin/bash
# This script updates the icon files from the svg file.
# It assumes that the svg file is square.
set -x
cd $(git rev-parse --show-toplevel)/assets/icon

src=terminaler-icon.svg

# the linux icon (128px PNG)
magick "$src" -background none -density 300 -resize 128x128 ../icon/terminal.png

# The Windows icon (multi-resolution ICO)
magick "$src" -background none -density 300 -define icon:auto-resize=256,128,96,64,48,32,16 ../windows/terminal.ico

