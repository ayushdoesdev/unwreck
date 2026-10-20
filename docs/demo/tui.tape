# Short TUI walkthrough. Record from the repo root (scripts/record-demo.sh).
Require unwreck
Require git
Require python3

Output docs/demo/tui.gif

Set Shell bash
Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1200
Set Height 600
Set Padding 20
Set TypingSpeed 50ms
Set Framerate 24
Set CursorBlink false
Set WindowBar Colorful
Set WindowBarSize 28
Set LoopOffset 35%
Env LANG "en_US.UTF-8"
Env LC_ALL "en_US.UTF-8"

Hide
Source docs/demo/setup.tape
Type@1ms `export FAKE_CODEX_SLEEP=0.05`
Enter
Type@1ms `codex "fix the failing tests"`
Enter
Wait+Screen@20s /- Done/
Wait+Line@10s /^\$/
Type@1ms `clear`
Enter
Sleep 200ms
Show

Sleep 300ms
Type "unwreck tui"
Enter
Sleep 1.8s
Tab
Sleep 1.2s
Down
Sleep 1s
Up
Sleep 1.2s
Type "q"
Sleep 800ms
