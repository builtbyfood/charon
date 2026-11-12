# Charon OSC 133 hook for bash. Sourced, not installed.
# BEL-terminated (\a): an ESC \ terminator inside PS1 collides with bash's own
# \[ \] prompt escapes and leaks a stray ']'. \033 rather than \e for dash/sh.
# Guarded by an if, not `&& return`: the hook arrives through `eval`, where a
# top-level return is an error ("can only `return' from a function or sourced
# script") and the rest would load a second time.
if [ -z "$CHARON_HOOKED" ]; then
CHARON_HOOKED=1
__charon_prompt_start() { printf '\033]133;A\a'; }
__charon_prompt_end()   { printf '\033]133;B\a'; }
__charon_preexec()      { printf '\033]133;C\a'; }
# D (exit code) then OSC 7 (cwd) so the SFTP pane can follow the shell.
__charon_precmd()       { printf '\033]133;D;%s\a\033]7;file://%s%s\a' "$?" "${HOSTNAME:-localhost}" "$PWD"; }
# preexec via DEBUG trap (only for interactive commands, not PROMPT_COMMAND itself)
__charon_debug() { [ -n "$COMP_LINE" ] && return; [ "$BASH_COMMAND" = "$PROMPT_COMMAND" ] && return; [ -n "$__charon_in_prompt" ] && return; __charon_preexec; }
trap '__charon_debug' DEBUG
PROMPT_COMMAND="__charon_in_prompt=1; __charon_precmd; ${PROMPT_COMMAND:+$PROMPT_COMMAND; }unset __charon_in_prompt"
PS1="\[$(__charon_prompt_start)\]${PS1}\[$(__charon_prompt_end)\]"
fi
