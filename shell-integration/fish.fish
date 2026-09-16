# Charon OSC 133 hook for fish. Idempotent: redefining the functions is harmless,
# so there is no early exit (an `exit` here would end an interactive shell).
# BEL-terminated OSC (see bash.sh).
set -g CHARON_HOOKED 1
function __charon_prompt --on-event fish_prompt; printf '\033]133;D;%s\a' $status; printf '\033]7;file://%s%s\a' $hostname $PWD; printf '\033]133;A\a'; end
function __charon_preexec --on-event fish_preexec; printf '\033]133;C\a'; end
