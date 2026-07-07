# xfr — README demo cassette (live throughput TUI)
#
# Rendered with VHS (https://github.com/charmbracelet/vhs):
#   cargo build
#   ./target/debug/xfr serve >/dev/null 2>&1 &          # throwaway local server
#   PATH="$PWD/target/debug:$PATH" vhs docs/demo.tape    # -> docs/demo.gif
#   kill %1                                              # stop the server
#
# The client runs a rate-limited (-b 1G) TCP test against a local `xfr serve`,
# so the graph reads like a clean gigabit link. No external network or creds.

Output "docs/demo.gif"

Set Width 1200
Set Height 720
Set FontSize 15
Set Padding 18
Set Theme "Catppuccin Mocha"
Set CursorBlink false

Require xfr

# --- Fresh throwaway HOME so the demo is deterministic (default theme, no saved
# prefs) and never touches real config. Not captured. -------------------------
Hide
Type "export HOME=$(mktemp -d)"
Enter
Sleep 300ms
Type "clear"
Enter
Sleep 300ms
Show

# --- Launch the client TUI (server already listening on 127.0.0.1:5201) -------
Type@40ms "xfr 127.0.0.1 -b 1G -t 60sec"
Enter
Sleep 6s          # connect + the live throughput graph fills
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
