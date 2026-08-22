#!/usr/bin/env bash
#
# webview-sidebar-check.sh — headless render check for assets/sidebar.html.
# Injects the committed state fixture into the real sidebar page, renders it
# at rail / wide / flyout / drawer states with headless Chromium, and drops
# screenshots into the given output directory (default: ./sidebar-check).
#
# Usage: ci/webview-sidebar-check.sh [outdir]
# Author: terminaler project
set -euo pipefail
OUT="${1:-sidebar-check}"; mkdir -p "$OUT"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CH="${CHROME:-$(command -v chromium || command -v google-chrome || ls "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux64/chrome 2>/dev/null | head -1)}"
[ -x "$CH" ] || { echo "no chromium found; set CHROME=" >&2; exit 1; }
python3 - "$ROOT" "$OUT" <<'PYEOF'
import pathlib, sys, json
root, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
html = (root / 'terminaler-gui/assets/sidebar.html').read_text()
fixture = (root / 'terminaler-gui/assets/mockups/sidebar-fixture.json').read_text()
schemes = json.dumps(["Gruvbox Dark (Gogh)", "Catppuccin Mocha", "Tokyo Night", "Nord"])
def harness(name, extra):
    inj = f"""<script>
window.ipc = {{ postMessage: function (m) {{ console.log('IPC', m); }} }};
ALL_SCHEMES = {schemes};
var FIXTURE = {fixture};
window.addEventListener('load', function () {{ window.__updateState(FIXTURE); {extra} }});
</script></body>"""
    (out / name).write_text(html.replace('</body>', inj))
pre = "_railWidth = 180; document.documentElement.style.setProperty('--rail-w','180px');"
harness('rail.html', '')
harness('flyout.html', pre + " openFlyout(tabFlyoutDesc(FIXTURE.tabs[1])(), 90);")
harness('theme.html', pre + " toggleDrawer('theme');")
harness('stats.html', pre + " toggleDrawer('stats');")
PYEOF
shot() { "$CH" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size="$2" --screenshot="$OUT/$3" "file://$(cd "$OUT" && pwd)/$1" 2>/dev/null; }
shot rail.html   180,760 rail-180.png
shot rail.html   280,760 rail-280.png
shot flyout.html 560,760 flyout.png
shot theme.html  560,760 drawer-theme.png
shot stats.html  560,760 drawer-stats.png
echo "screenshots in $OUT/"; ls -la "$OUT"/*.png
