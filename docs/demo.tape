# xfr — README demo cassette (live throughput TUI)
#
# Render on Linux from the repo root (needs vhs, ttyd, ffmpeg). Preferences
# are isolated via XDG_CONFIG_HOME, which dirs::config_dir() ignores on macOS.
#   cargo build
#   xfr_demo_server_dir=$(mktemp -d)
#   XDG_CONFIG_HOME="$xfr_demo_server_dir" ./target/debug/xfr serve --bind 127.0.0.1 --no-mdns -p 45201 >/dev/null 2>&1 &
#   xfr_demo_server_pid=$!
#   VHS_NO_SANDBOX=1 PATH="$HOME/.cache/rod/browser/chromium-1321438:$PWD/target/debug:$HOME/go/bin:$HOME/.local/bin:$PATH" vhs docs/demo.tape
#   kill "$xfr_demo_server_pid"
#   rm -rf "$xfr_demo_server_dir"
#
# vhs: `go install github.com/charmbracelet/vhs@v0.11.0` (lands in ~/go/bin);
# ttyd: a release binary from github.com/tsl0922/ttyd in ~/.local/bin.
# VHS 0.11 (go-rod 0.116) hangs opening its page on current Chrome, so it is
# pointed at rod's pinned Chromium 128 (picked up as `chrome` on PATH); fetch once:
#   curl -LO https://storage.googleapis.com/chromium-browser-snapshots/Linux_x64/1321438/chrome-linux.zip
#   unzip chrome-linux.zip && mkdir -p ~/.cache/rod/browser && mv chrome-linux ~/.cache/rod/browser/chromium-1321438
# VHS_NO_SANDBOX is needed where unprivileged user namespaces are restricted
# (Ubuntu AppArmor).
#
# Launch and quit are hidden, so the GIF starts and ends on the populated TUI.
# The client runs a rate-limited (-b 1G) TCP test against a local `xfr serve`,
# so the graph reads like a clean gigabit link. No external network or creds.

Output "docs/demo.gif"

# 1x size: at 2x VHS drops frames and the GIF plays ~2x fast. The short height
# still fits the 19-row Settings modal without a mostly-empty History pane.
Set Width 1200
Set Height 540
Set FontSize 15
Set Padding 18
Set Theme "Catppuccin Mocha"
Set CursorBlink false

Require xfr

# --- Isolated preferences/cache; keep the user's HOME intact. ----------------
# NO_COLOR in the recording shell would strip the theme's colours (crossterm
# honours it even with an explicit --theme), so clear it.
Hide
Type "unset NO_COLOR"
Enter
Type "export XFR_DEMO_DIR=$(mktemp -d)"
Enter
Type "export XDG_CONFIG_HOME=$XFR_DEMO_DIR/config XDG_CACHE_HOME=$XFR_DEMO_DIR/cache"
Enter
Type "trap 'rm -rf $XFR_DEMO_DIR' EXIT"
Enter
Sleep 300ms
Type "clear"
Enter
Sleep 300ms
# --- Launch off-camera; the graph has data before the first visible frame. ---
Type "xfr 127.0.0.1 -p 45201 -b 1G -t 60sec --no-update-check --theme default"
Enter
Sleep 6s          # connect + the live throughput graph fills
Show
Sleep 5s          # hold as per-second interval updates animate the graph

# --- Peek at the Settings modal (theme picker + the update-check toggle) -------
Type "s"
Sleep 1200ms
Down
Sleep 400ms
Down
Sleep 400ms
Down
Sleep 2500ms      # rest on the "Update check" row
Type "s"
Sleep 600ms

# --- Final hold on the live graph, then quit OFF-camera -----------------------
Sleep 4s
Hide
Type "q"
Sleep 500ms
Type "exit"
Enter
