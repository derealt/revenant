#!/usr/bin/env zsh
# REVENANT — Zsh Shell Integration
#
# Source this in your .zshrc:
#   [ -f ~/.config/revenant/revenant.zsh ] && source ~/.config/revenant/revenant.zsh
#
# On every new shell session, checks for a ghost MOTD from the daemon
# and displays it. The ghost shows what you were doing when you last
# left this project, and what your next step was.

REVENANT_MOTD="${HOME}/.revenant/motd"
REVENANT_META="${HOME}/.revenant/motd.json"
REVENANT_ACTIVE_FILE="${HOME}/.revenant/terminal-active"

# ─── Display ghost on shell startup ─────────────────────────────

__revenant_show_ghost() {
    if [[ -f "${REVENANT_MOTD}" ]]; then
        # Check if the ghost has expired
        if [[ -f "${REVENANT_META}" ]]; then
            local expires_at
            expires_at=$(python3 -c "
import json, sys
from datetime import datetime, timezone
try:
    with open('${REVENANT_META}') as f:
        meta = json.load(f)
    exp = datetime.fromisoformat(meta['expires_at'].replace('Z', '+00:00'))
    now = datetime.now(timezone.utc)
    if now < exp:
        print('active')
    else:
        print('expired')
except:
    print('active')
" 2>/dev/null)

            if [[ "${expires_at}" == "expired" ]]; then
                rm -f "${REVENANT_MOTD}" "${REVENANT_META}" 2>/dev/null
                return
            fi
        fi

        # Display the ghost
        echo ""
        cat "${REVENANT_MOTD}"
        echo ""

        # Record that we showed the ghost (for activity tracking)
        date +%s > "${REVENANT_ACTIVE_FILE}" 2>/dev/null
    fi
}

# ─── Track directory changes for context switch detection ────────

__revenant_track_cd() {
    local new_dir="$(pwd)"
    if [[ -n "${REVENANT_LAST_DIR}" && "${new_dir}" != "${REVENANT_LAST_DIR}" ]]; then
        # Directory changed — notify daemon via state file
        echo "${new_dir}" > "${HOME}/.revenant/cwd" 2>/dev/null
    fi
    REVENANT_LAST_DIR="${new_dir}"
}

# ─── Activity heartbeat ─────────────────────────────────────────

__revenant_heartbeat() {
    # Touch the active file on each command to track activity
    if [[ -f "${REVENANT_ACTIVE_FILE}" ]]; then
        local ghost_start
        ghost_start=$(cat "${REVENANT_ACTIVE_FILE}" 2>/dev/null)
        local now=$(date +%s)
        local elapsed=$((now - ghost_start))

        # Read TTL from metadata (default 120s if missing)
        local ttl=120
        if [[ -f "${REVENANT_META}" ]]; then
            local meta_ttl
            meta_ttl=$(python3 -c "import json; print(json.load(open('${REVENANT_META}'))['ttl_seconds'])" 2>/dev/null)
            [[ -n "${meta_ttl}" ]] && ttl="${meta_ttl}"
        fi

        # If we've been active past the TTL, clear the ghost
        if [[ ${elapsed} -ge ${ttl} ]]; then
            rm -f "${REVENANT_MOTD}" "${REVENANT_META}" "${REVENANT_ACTIVE_FILE}" 2>/dev/null
        fi
    fi
}

# ─── Hook into zsh ──────────────────────────────────────────────

# Show ghost on startup
__revenant_show_ghost

# Track directory changes
if [[ -z "${REVENANT_LAST_DIR}" ]]; then
    REVENANT_LAST_DIR="$(pwd)"
fi

# Add to chpwd hook (fires on directory change)
autoload -Uz add-zsh-hook
add-zsh-hook chpwd __revenant_track_cd

# Add heartbeat to precmd (fires before each prompt)
add-zsh-hook precmd __revenant_heartbeat
