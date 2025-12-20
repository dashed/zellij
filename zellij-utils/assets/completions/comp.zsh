# =============================================================================
# Zellij ZSH Dynamic Session Completion
# =============================================================================

# Get list of active session names for completion
_zellij_sessions() {
    local -a sessions
    sessions=("${(@f)$(zellij list-sessions --short --no-formatting 2>/dev/null)}")
    sessions=(${sessions:#})  # Remove empty entries
    if (( ${#sessions} )); then
        _describe -t sessions 'zellij session' sessions
    fi
}

# =============================================================================
# Convenience functions
# =============================================================================

function zr () { zellij run --name "$*" -- zsh -ic "$*";}
function zrf () { zellij run --name "$*" --floating -- zsh -ic "$*";}
function zri () { zellij run --name "$*" --in-place -- zsh -ic "$*";}
function ze () { zellij edit "$*";}
function zef () { zellij edit --floating "$*";}
function zei () { zellij edit --in-place "$*";}
function zpipe () {
  if [ -z "$1" ]; then
    zellij pipe;
  else
    zellij pipe -p $1;
  fi
}
