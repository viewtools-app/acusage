# Shared settings and hidden setup for every demo tape. Sourced by the other tapes.
# DEMO_HOME and DEMO_BIN are set by scripts/demo/record.sh.
Set Shell "bash"
Set FontSize 14
Set Width 1260
Set Padding 16
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set CursorBlink false

Hide
Type 'export HOME="$DEMO_HOME" PATH="$DEMO_BIN:$PATH" TZ=UTC; unset CLAUDE_CONFIG_DIR CODEX_HOME ACUSAGE_CONFIG; export PS1="$ "; clear'
Enter
Sleep 500ms
Show
