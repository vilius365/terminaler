#!/usr/bin/env bash
# build-windows-staging.sh — cross-build Terminaler for Windows into a pCloud
#                            STAGING folder, without touching the live install.
#
# Description: ~/pCloudDrive/terminaler-windows-build/ IS the folder the Windows
#              exes run from, so building straight into it requires quitting
#              Terminaler first. This script builds into a sibling staging
#              folder instead, which syncs to Windows harmlessly while you keep
#              working. You then run the promote step (or P:\promote.cmd on
#              Windows) once, when you are ready to restart Terminaler.
#
# Usage:
#   ci/build-windows-staging.sh                 build + stage
#   ci/build-windows-staging.sh --promote       promote staging -> live (devbox side)
#   ci/build-windows-staging.sh --dry-run       show what would happen
#   ci/build-windows-staging.sh --status        compare staging vs live
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
LIVE_DIR="$PCLOUD_DIR/terminaler-windows-build"
STAGE_DIR="$PCLOUD_DIR/terminaler-windows-staging"
TARGET="x86_64-pc-windows-gnu"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO_ROOT/target/$TARGET/release"

# The three exes plus the support files that must travel with them.
EXES=(terminaler-gui.exe terminaler-mux-server.exe terminaler.exe)
SUPPORT=(WebView2Loader.dll conpty.dll OpenConsole.exe)

DRY_RUN=false; PROMOTE=false; STATUS=false
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=true; shift ;;
        --promote) PROMOTE=true; shift ;;
        --status)  STATUS=true; shift ;;
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -d "$PCLOUD_DIR" ] || die "pCloud mount not found at $PCLOUD_DIR"

show_status() {
    log_info "live:    $LIVE_DIR"
    log_info "staging: $STAGE_DIR"
    local f
    for f in "${EXES[@]}"; do
        local live_t="missing" stage_t="missing"
        [ -f "$LIVE_DIR/$f" ]  && live_t="$(date -r "$LIVE_DIR/$f"  '+%Y-%m-%d %H:%M')"
        [ -f "$STAGE_DIR/$f" ] && stage_t="$(date -r "$STAGE_DIR/$f" '+%Y-%m-%d %H:%M')"
        printf '  %-28s live: %-17s staging: %s\n' "$f" "$live_t" "$stage_t"
    done
}

if $STATUS; then show_status; exit 0; fi

# --- promote: staging -> live. The only step that needs Terminaler closed. ---
if $PROMOTE; then
    [ -d "$STAGE_DIR" ] || die "no staging folder at $STAGE_DIR — build first"
    for f in "${EXES[@]}"; do
        [ -f "$STAGE_DIR/$f" ] || die "staging is incomplete: $f missing — rebuild"
    done
    if pgrep -f 'terminaler-(gui|mux-server)' >/dev/null 2>&1; then
        log_warn "a terminaler process is running ON THIS MACHINE; on Windows make sure"
        log_warn "Terminaler AND terminaler-mux-server.exe are fully stopped first."
    fi
    BACKUP_DIR="$LIVE_DIR/backup-$(date '+%Y%m%d-%H%M%S')"
    if $DRY_RUN; then
        log_info "dry-run — would do:"
        echo "  mkdir -p $BACKUP_DIR"
        for f in "${EXES[@]}" "${SUPPORT[@]}"; do
            [ -f "$LIVE_DIR/$f" ] && echo "  cp $LIVE_DIR/$f -> $BACKUP_DIR/"
        done
        for f in "${EXES[@]}" "${SUPPORT[@]}"; do
            [ -f "$STAGE_DIR/$f" ] && echo "  cp $STAGE_DIR/$f -> $LIVE_DIR/"
        done
        exit 0
    fi
    mkdir -p "$BACKUP_DIR"
    for f in "${EXES[@]}" "${SUPPORT[@]}"; do
        [ -f "$LIVE_DIR/$f" ] && cp -p "$LIVE_DIR/$f" "$BACKUP_DIR/"
    done
    log_info "backed up current live exes -> $BACKUP_DIR"
    for f in "${EXES[@]}" "${SUPPORT[@]}"; do
        [ -f "$STAGE_DIR/$f" ] && cp -p "$STAGE_DIR/$f" "$LIVE_DIR/"
    done
    log_info "promoted staging -> live. Relaunch terminaler-gui.exe on Windows."
    exit 0
fi

# --- build + stage ---
command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH"
command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 \
    || die "mingw toolchain missing (dnf install --enablerepo=crb mingw64-gcc mingw64-gcc-c++)"

if $DRY_RUN; then
    log_info "dry-run — would do:"
    echo "  cargo build --release --target $TARGET -p terminaler-gui -p terminaler-mux-server -p terminaler"
    echo "  mkdir -p $STAGE_DIR"
    for f in "${EXES[@]}" "${SUPPORT[@]}"; do echo "  cp $OUT/$f -> $STAGE_DIR/"; done
    exit 0
fi

log_info "cross-building for $TARGET (this does NOT touch the live install)"
cargo build --release --target "$TARGET" \
    -p terminaler-gui -p terminaler-mux-server -p terminaler

mkdir -p "$STAGE_DIR"
for f in "${EXES[@]}"; do
    [ -f "$OUT/$f" ] || die "expected build output missing: $OUT/$f"
    cp -p "$OUT/$f" "$STAGE_DIR/"
done
# Support files: ship them when the build produced them, else carry the live copy
# forward so staging is always a COMPLETE, promotable set.
for f in "${SUPPORT[@]}"; do
    if [ -f "$OUT/$f" ]; then
        cp -p "$OUT/$f" "$STAGE_DIR/"
    elif [ -f "$LIVE_DIR/$f" ]; then
        cp -p "$LIVE_DIR/$f" "$STAGE_DIR/"
        log_warn "$f not in build output — carried the live copy into staging"
    else
        log_warn "$f missing from both build output and live install"
    fi
done

{
    echo "Terminaler Windows staging build"
    echo "built:  $(date '+%Y-%m-%d %H:%M:%S') on $(hostname)"
    echo "commit: $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "branch: $(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
    if [ -n "$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null)" ]; then
        echo "tree:   DIRTY — these bytes do not match the commit above"
    else
        echo "tree:   clean"
    fi
    echo
    echo "sha256:"
    (cd "$STAGE_DIR" && sha256sum "${EXES[@]}" 2>/dev/null)
} > "$STAGE_DIR/BUILD-INFO.txt"

log_info "staged -> $STAGE_DIR"
log_info "Terminaler is still running and untouched. When ready to update:"
log_info "  quit Terminaler + terminaler-mux-server.exe, then:"
log_info "  ci/build-windows-staging.sh --promote"
