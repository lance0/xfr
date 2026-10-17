# xfr — README demo cassette (live throughput TUI)
#
# Rendered with VHS (https://github.com/charmbracelet/vhs):
#   cargo build
#   xfr_demo_server_dir=$(mktemp -d)
#   XDG_CONFIG_HOME="$xfr_demo_server_dir" ./target/debug/xfr serve --bind 127.0.0.1 --no-mdns -p 45201 >/dev/null 2>&1 &
#   xfr_demo_server_pid=$!
#   VHS_NO_SANDBOX=1 PATH="$HOME/.cache/rod/browser/chromium-1321438:$PWD/target/debug:$HOME/go/bin:$HOME/.local/bin:$PATH" vhs docs/demo.tape
#   kill "$xfr_demo_server_pid"
#   rm -rf "$xfr_demo_server_dir"
#
# The pinned Chromium on PATH avoids VHS 0.11/go-rod hangs with newer Chrome.
# Launch and quit are hidden, so the GIF starts and ends on the populated TUI.
#
# The client runs a rate-limited (-b 1G) TCP test against a local `xfr serve`,
# so the graph reads like a clean gigabit link. No external network or creds.

Output "docs/demo.gif"

# Rendered at 2x (was 1200x720 / FontSize 15). Same 127x36 terminal, twice the
# pixels per character cell, so the block glyphs in the throughput graph and the
# transfer bar stay sharp when GitHub scales the image down.
Set Width 2400
Set Height 1440
Set FontSize 30
Set Padding 36
Set Theme "Catppuccin Mocha"
Set CursorBlink false

Require xfr

# --- Isolated preferences/cache; keep the user's HOME intact. ----------------
Hide
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
