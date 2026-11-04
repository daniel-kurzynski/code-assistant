#!/usr/bin/env bash
# Ubuntu/Debian packages capture.py needs, on top of what building the app
# needs (see .github/actions/setup-rust/action.yml).
set -euo pipefail
sudo=""
[ "$(id -u)" -eq 0 ] || sudo="sudo"
$sudo apt-get update
$sudo apt-get install -y --no-install-recommends \
  xvfb xdotool x11-xserver-utils imagemagick \
  mesa-vulkan-drivers libvulkan1 \
  fonts-inter fonts-jetbrains-mono \
  python3 python3-yaml python3-fonttools \
  git
