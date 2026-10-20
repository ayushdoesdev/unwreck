# Launch demo: scripted agent wipes files, then unwreck undo. Record from the repo root
# (see scripts/record-demo.sh). Target: ~28–30s, GIF under 1.5 MB.
# No LoopOffset: GitHub's first-frame preview must be the clean prompt.
Require unwreck
Require git
Require python3

Output docs/demo/demo.gif
Output docs/demo/demo.mp4

Set Shell bash
Set Theme "Catppuccin Mocha"
Set FontSize 18
Set Width 1200
Set Height 600
Set Padding 20
Set TypingSpeed 50ms
Set Framerate 24
Set PlaybackSpeed 1.1
Set CursorBlink false
Set WindowBar Colorful
Set WindowBarSize 28
Env LANG "en_US.UTF-8"
Env LC_ALL "en_US.UTF-8"

Hide
Source docs/demo/setup.tape
Ctrl+L
Sleep 500ms
Show

Sleep 400ms
Type "ls src/gen .env"
Enter
Sleep 800ms
Type `codex "fix the failing tests"`
Enter
Wait+Screen@20s /- Done/
Wait+Line@10s /^\$/
Sleep 400ms
Type "ls src/gen .env"
Enter
Sleep 1s
Type "# 💥 the agent just wiped src/gen and .env"
Enter
Sleep 400ms
Type "unwreck log"
Enter
Sleep 3s
Type "unwreck diff"
Enter
Sleep 2.5s
Type "# ⏪ one command to get it back"
Enter
Sleep 400ms
Type "unwreck undo"
Enter
Sleep 2.5s
Type "ls src/gen .env"
Enter
Sleep 4s
