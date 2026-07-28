# cc-loadout README demo — hub TUI tab tour (Overview → Accounts → Schedule →
# Profile board/Detail/Rules → Tasks → wrap). Rendered by demo/record.sh, which
# substitutes __FX__ with <repo>/target/demo-fixture into target/demo.tape.
# Do not run this file directly.

Output docs/assets/demo.gif

Require cc-loadout
Require ttyd
Require ffmpeg

Set Shell "bash"
Set FontSize 14
Set Width 1200
Set Height 660
Set Padding 12
Set WindowBar Colorful
Set Theme "GruvboxDark"

Env HOME "__FX__"
Env XDG_DATA_HOME "__FX__/.local/share"
Env PATH "__FX__/bin:/usr/local/bin:/usr/bin:/bin"

Hide
Type "cd __FX__/repos/api-server && clear"
Enter
Sleep 800ms
Show

Sleep 400ms
Type@60ms "cc-loadout"
Sleep 300ms
Enter
# Overview
Sleep 1800ms
# Accounts — hop in, nudge the cursor between the two accounts
Tab
Sleep 800ms
Down
Sleep 600ms
Up
Sleep 500ms
# Schedule
Tab
Sleep 1300ms
# Profile — by-plugin: run the cursor down the list
Tab
Sleep 600ms
Down
Sleep 180ms
Down
Sleep 180ms
Down
Sleep 180ms
Down
Sleep 450ms
# by-profile board (Universal / profiles / On-demand)
Type "v"
Sleep 800ms
Down
Sleep 220ms
Down
Sleep 450ms
# open a profile's Detail
Enter
Sleep 1000ms
# Rules tab inside Detail — the standout: live match count + near-miss
Tab
Sleep 2200ms
# back to the board
Escape
Sleep 700ms
# Tasks
Tab
Sleep 1100ms
# wrap to Overview for a clean loop
Tab
Sleep 1500ms
