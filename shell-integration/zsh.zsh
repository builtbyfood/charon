# Charon OSC 133 hook for zsh. BEL-terminated OSC (see bash.sh).
[[ -n "$CHARON_HOOKED" ]] && return; CHARON_HOOKED=1
autoload -Uz add-zsh-hook
__charon_precmd()  { print -Pn '\033]133;D;%?\a'; printf '\033]7;file://%s%s\a' "${HOST:-localhost}" "$PWD"; PS1="%{$(printf '\033]133;A\a')%}${PS1}%{$(printf '\033]133;B\a')%}"; }
__charon_preexec() { printf '\033]133;C\a'; }
add-zsh-hook precmd  __charon_precmd
add-zsh-hook preexec __charon_preexec
