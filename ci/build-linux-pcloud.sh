#!/usr/bin/env bash
# build-linux-pcloud.sh — build Terminaler for Linux and copy it to pCloud.
#
# Description: Native Linux counterpart to build-windows-staging.sh. Builds the
#              three release binaries and copies them, plus a generated
#              DEPLOY-README, into ~/pCloudDrive/terminaler-linux-build/.
#
#              Unlike the Windows flow there is no staging/promote split: on
#              Linux nothing locks a running executable, so the copy is the
#              deploy. It does refuse to overwrite binaries that are currently
#              running from the pCloud folder itself.
#
#              PORTABILITY: these binaries are dynamically linked (glibc plus
#              ~28 shared libraries). They run on a machine whose glibc is at
#              least the build host's and which has the runtime libs installed.
#              A Fedora 44 target with a newer glibc is fine; an OLDER target is
#              not. If the binary refuses to start there, build natively on that
#              machine from source instead — see DEPLOY-README.txt.
#
# Usage:
#   ci/build-linux-pcloud.sh              build + copy
#   ci/build-linux-pcloud.sh --dry-run    show what would happen
#   ci/build-linux-pcloud.sh --status     compare built vs deployed
#   ci/build-linux-pcloud.sh --no-build   copy existing target/release output
#
# Env:   PCLOUD_DIR (default ~/pCloudDrive)
# Exit:  0 on success, 1 on error
# Author: INVADE Team
#
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
log_info()  { echo -e "${GREEN}[build]${NC} $*"; }
log_warn()  { echo -e "${YELLOW}[build]${NC} $*"; }
log_error() { echo -e "${RED}[build]${NC} $*" >&2; }
die() { log_error "$*"; exit 1; }

PCLOUD_DIR="${PCLOUD_DIR:-$HOME/pCloudDrive}"
DEST_DIR="$PCLOUD_DIR/terminaler-linux-build"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO_ROOT/target/release"

BINS=(terminaler-gui terminaler-mux-server terminaler)

DRY_RUN=false; STATUS=false; NO_BUILD=false
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)  DRY_RUN=true; shift ;;
        --status)   STATUS=true; shift ;;
        --no-build) NO_BUILD=true; shift ;;
        -h|--help)  sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -d "$PCLOUD_DIR" ] || die "pCloud mount not found at $PCLOUD_DIR"

show_status() {
    log_info "built:    $OUT"
    log_info "deployed: $DEST_DIR"
    local f built_t dep_t
    for f in "${BINS[@]}"; do
        built_t="missing"; dep_t="missing"
        [ -f "$OUT/$f" ]      && built_t="$(date -r "$OUT/$f"      '+%Y-%m-%d %H:%M')"
        [ -f "$DEST_DIR/$f" ] && dep_t="$(date -r "$DEST_DIR/$f"   '+%Y-%m-%d %H:%M')"
        printf '  %-24s built: %-17s deployed: %s\n' "$f" "$built_t" "$dep_t"
    done
}

if $STATUS; then show_status; exit 0; fi

# Refuse to clobber a binary that is executing from the destination itself;
# overwriting it in place would corrupt the running process's text pages.
for f in "${BINS[@]}"; do
    if pgrep -f "^$DEST_DIR/$f" >/dev/null 2>&1; then
        die "$f is running from $DEST_DIR — close it before deploying"
    fi
done

if $DRY_RUN; then
    log_info "dry-run — would do:"
    $NO_BUILD || echo "  cargo build --release -p terminaler-gui -p terminaler-mux-server -p terminaler"
    echo "  mkdir -p $DEST_DIR"
    for f in "${BINS[@]}"; do echo "  cp $OUT/$f -> $DEST_DIR/"; done
    echo "  write $DEST_DIR/DEPLOY-README.txt"
    exit 0
fi

if ! $NO_BUILD; then
    command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH"
    log_info "building release binaries (X11 + Wayland)"
    cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" \
        -p terminaler-gui -p terminaler-mux-server -p terminaler
fi

mkdir -p "$DEST_DIR"
for f in "${BINS[@]}"; do
    [ -f "$OUT/$f" ] || die "expected build output missing: $OUT/$f (drop --no-build?)"
    cp -p "$OUT/$f" "$DEST_DIR/"
    log_info "copied $f ($(du -h "$OUT/$f" | cut -f1))"
done

BUILD_OS="$( (. /etc/os-release 2>/dev/null && printf '%s' "$PRETTY_NAME") || printf 'unknown')"
[ -n "$BUILD_OS" ] || BUILD_OS="unknown"
# `ldd --version` prints a trailing version on line 1; tolerate its absence.
BUILD_GLIBC="$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
[ -n "$BUILD_GLIBC" ] || BUILD_GLIBC="unknown"
NEED_GLIBC="$(objdump -T "$OUT/terminaler-gui" 2>/dev/null \
    | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -uV | tail -1 | sed 's/GLIBC_//' || true)"
[ -n "$NEED_GLIBC" ] || NEED_GLIBC="unknown"

cat > "$DEST_DIR/DEPLOY-README.txt" <<EOF
Terminaler — Linux build
========================
built:  $(date '+%Y-%m-%d %H:%M:%S') on $(hostname) ($BUILD_OS)
commit: $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
glibc:  built against $BUILD_GLIBC, binaries require >= $NEED_GLIBC

Contents
--------
  terminaler-gui         GUI client (X11 + Wayland, chosen at runtime)
  terminaler-mux-server  background daemon holding PTY sessions
  terminaler             CLI

Install the runtime libraries, then run
---------------------------------------
Fedora / RHEL derivatives:
  sudo dnf install libxcb xcb-util xcb-util-image xcb-util-keysyms \\
      xcb-util-wm libxkbcommon libxkbcommon-x11 libX11 wayland-libs \\
      mesa-libEGL fontconfig freetype harfbuzz openssl-libs

Debian / Ubuntu:
  sudo apt install libxcb1 libxcb-util1 libxcb-image0 libxcb-keysyms1 \\
      libxcb-icccm4 libxkbcommon0 libxkbcommon-x11-0 libx11-6 \\
      libwayland-client0 libegl1 libfontconfig1 libfreetype6 \\
      libharfbuzz0b libssl3

Then:
  chmod +x terminaler-gui terminaler-mux-server terminaler
  ./terminaler-gui

Config lives at ~/.config/terminaler/terminaler.json (generated on first run).
The mux socket is \$XDG_RUNTIME_DIR/terminaler/sock.

If it does not start
--------------------
These binaries are NOT portable across arbitrary distros. On a glibc or
soname mismatch ("version \`GLIBC_2.xx' not found", "libfoo.so.N: cannot open
shared object file"), build natively on that machine instead — it takes about
four minutes and is the supported path:

  git clone git@github.com:vilius365/terminaler.git
  cd terminaler
  # build dependencies are listed in README.md
  cargo build --release
  ./target/release/terminaler-gui

Verify without a display (checks the mux, not the GUI):
  ./terminaler-mux-server &
  export TERMINALER_UNIX_SOCKET="\$XDG_RUNTIME_DIR/terminaler/sock"
  ./terminaler cli list

A GUI run with no \$DISPLAY correctly exits with
"XOpenDisplay failed to open a display" — that means the X11 backend is live.
EOF

log_info "wrote $DEST_DIR/DEPLOY-README.txt"
log_info "deployed to $DEST_DIR (pCloud will sync it)"
