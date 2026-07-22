#!/usr/bin/env fish
# REVENANT - Fish Shell Integration
#
# Source this in your config.fish:
#   [ -f ~/.config/revenant/revenant.fish ] && source ~/.config/revenant/revenant.fish
#
# On every new shell session, checks for a ghost MOTD from the daemon
# and displays it. The ghost shows what you were doing when you last
# left this project, and what your next step was.

set -g REVENANT_MOTD "$HOME/.revenant/motd"
set -g REVENANT_META "$HOME/.revenant/motd.json"
set -g REVENANT_ACTIVE_FILE "$HOME/.revenant/terminal-active"

# --- Display ghost on shell startup -----------------------------

function __revenant_show_ghost
    if test -f "$REVENANT_MOTD"
        # Check if the ghost has expired
        if test -f "$REVENANT_META"
            set -l expired (python3 -c "
import json, sys
from datetime import datetime, timezone
try:
    with open('$REVENANT_META') as f:
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

            if test "$expired" = "expired"
                rm -f "$REVENANT_MOTD" "$REVENANT_META" 2>/dev/null
                return
            end
        end

        # Display the ghost
        echo ""
        cat "$REVENANT_MOTD"
        echo ""

        # Record that we showed the ghost (for activity tracking)
        date +%s > "$REVENANT_ACTIVE_FILE" 2>/dev/null
    end
end

# --- Track directory changes for context switch detection --------

function __revenant_track_cd --on-variable PWD
    if set -q REVENANT_LAST_DIR
        if test "$PWD" != "$REVENANT_LAST_DIR"
            # Directory changed -- notify daemon via state file
            echo "$PWD" > "$HOME/.revenant/cwd" 2>/dev/null
        end
    end
    set -g REVENANT_LAST_DIR "$PWD"
end

# --- Activity heartbeat -----------------------------------------

function __revenant_heartbeat --on-event fish_preexec
    if test -f "$REVENANT_ACTIVE_FILE"
        set -l ghost_start (cat "$REVENANT_ACTIVE_FILE" 2>/dev/null)
        set -l now (date +%s)

        if test -n "$ghost_start"
            set -l elapsed (math $now - $ghost_start)

            # If we've been active for 5+ minutes, clear the ghost
            if test $elapsed -ge 300
                rm -f "$REVENANT_MOTD" "$REVENANT_META" "$REVENANT_ACTIVE_FILE" 2>/dev/null
            end
        end
    end
end

# --- Initialize --------------------------------------------------

# Show ghost on startup
__revenant_show_ghost

# Track the initial directory
set -g REVENANT_LAST_DIR "$PWD"
