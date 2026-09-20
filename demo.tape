# ffkit demo tape. Render with:
#   vhs demo.tape        # needs vhs + ttyd + ffmpeg + X (xvfb) on PATH
# Produces demo.gif next to this file (committed for the README).
#
# The demo: pick Convert, choose holiday.mp4, lower CRF 23 → 20 while the
# command rebuilds live, peek at the command pane, quit.

Output "demo.gif"

Require ffmpeg

Set Shell "bash"
Set FontSize 18
Set Width 1280
Set Height 800
Set Framerate 12

Hide
Type "./ffkit" Enter
Sleep 4s
Show

Enter
Sleep 1s
Space
Sleep 500ms
Enter
Sleep 2s

Down
Sleep 300ms
Down
Sleep 300ms
Down
Sleep 300ms
Left
Sleep 300ms
Left
Sleep 300ms
Left
Sleep 1s

Tab
Sleep 1s

Escape
Sleep 500ms
Escape
Sleep 500ms
Type "q"
Sleep 1s
