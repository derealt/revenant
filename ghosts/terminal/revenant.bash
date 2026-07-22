#!/usr/bin/env bash
# REVENANT - Bash Shell Integration
#
# Source this in your .bashrc:
#   [ -f ~/.config/revenant/revenant.bash ] && source ~/.config/revenant/revenant.bash

REVENANT_MOTD="${HOME}/.revenant/motd"
REVENANT_META="${HOME}/.revenant/motd.json"
REVENANT_ACTIVE_FILE="${HOME}/.revenant/terminal-active"

__revenant_show_ghost() {
    # A new shell is itself a return signal: announce it to the daemon,
    # which decides (by its own absence rules) whether this shell
    # deserves a ghost. The daemon answers with rendered MOTD text.
    local ghost cwd_json
    cwd_json="${PWD//\\/\\\\}"
    cwd_json="${cwd_json//\"/\\\"}"
    ghost=$(curl -fsS --max-time 0.5 -X POST -H 'Content-Type: application/json' \
        --data "{\"cwd\":\"${cwd_json}\"}" http://127.0.0.1:7711/shell-open 2>/dev/null)
    if [[ -n "${ghost}" ]]; then
        echo ""
        printf '%s\n' "${ghost}"
        echo ""
        date +%s > "${REVENANT_ACTIVE_FILE}" 2>/dev/null
        return
    fi

    # Daemon unreachable (or chose silence): fall back to the MOTD file
    if [[ -f "${REVENANT_MOTD}" ]]; then
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

        echo ""
        cat "${REVENANT_MOTD}"
        echo ""

        date +%s > "${REVENANT_ACTIVE_FILE}" 2>/dev/null
    fi
}

__revenant_heartbeat() {
    if [[ -f "${REVENANT_ACTIVE_FILE}" ]]; then
        local ghost_start
        ghost_start=$(cat "${REVENANT_ACTIVE_FILE}" 2>/dev/null)
        local now=$(date +%s)
        local elapsed=$((now - ghost_start))

        if [[ ${elapsed} -ge 300 ]]; then
            rm -f "${REVENANT_MOTD}" "${REVENANT_META}" "${REVENANT_ACTIVE_FILE}" 2>/dev/null
        fi
    fi

    # Track directory changes
    local new_dir="$(pwd)"
    if [[ -n "${REVENANT_LAST_DIR}" && "${new_dir}" != "${REVENANT_LAST_DIR}" ]]; then
        echo "${new_dir}" > "${HOME}/.revenant/cwd" 2>/dev/null
    fi
    REVENANT_LAST_DIR="${new_dir}"
}

# Show ghost on startup
__revenant_show_ghost

REVENANT_LAST_DIR="$(pwd)"
PROMPT_COMMAND="__revenant_heartbeat${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
