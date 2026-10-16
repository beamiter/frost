# Test-only startup file. No personal startup files or shell history are used.
set +o history
set -o emacs
bind 'set enable-bracketed-paste off'
bind 'set bell-style none'
_frost_readline_snapshot() {
    # Observe readline's real editor, without assigning either variable (which
    # would itself alter the undo list). Rename makes the report atomic.
    printf '%s\n%s\n' "$READLINE_POINT" "$READLINE_LINE" > "$FROST_READLINE_REPORT/$$.tmp"
    mv "$FROST_READLINE_REPORT/$$.tmp" "$FROST_READLINE_REPORT/$$"
}
bind -x '"\C-x\C-d":_frost_readline_snapshot'
PS1=$'\\[\e]133;A\a\\]$ \\[\e]133;B\a\\]'
PS2='> '
PROMPT_COMMAND=
# A real completed transcript, followed by bash's real unsubmitted prompt.
printf '\033]133;A\007$ \033]133;B\007printf needle\r\n\033]133;C\007'
for ((i=0; i<80; i++)); do printf 'needle history %03d: a long retained line for width normalization after a real split\r\n' "$i"; done
printf '\033]133;D;0\007'
