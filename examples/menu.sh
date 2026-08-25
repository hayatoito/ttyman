#!/usr/bin/env bash
# ==============================================================================
# ttyman Interactive Session Switcher & Manager (FZF Menu)
# ==============================================================================
# Features:
#   - Mark current session with '*' indicator (via $TTYMAN_SESSION)
#   - Live screen preview with ANSI colors on hover
#   - Instant session switching (Enter)
#   - Dynamic new session creation (type any new name + Enter)
#   - Interactive session rename (Ctrl-R)
#   - Terminate / kill session from menu by PID (Ctrl-X)
#   - Cancel and restore terminal cleanly (ESC)
# ==============================================================================
set -eo pipefail

# 1. Preview helper: invoked by fzf
if [ "${1:-}" = "--preview" ]; then
    raw_item="${2:-}"
    query="${3:-}"
    item=$(echo "$raw_item" | sed -E 's/^[ *][ ]//')

    if [ -z "$item" ] || [ "$item" = "$query" ] && ! ttyman list --json 2>/dev/null | jq -e -r --arg n "$item" '.[].name | select(. == $n)' >/dev/null 2>&1; then
        if [ -n "$query" ]; then
            echo -e "\033[1;32m[+ New Session: $query]\033[0m"
            echo ""
            echo "Press Enter to spawn and attach to '$query'."
        else
            echo "Select an active session or type a new session name."
        fi
        exit 0
    fi

    if [ "$item" = "[detach]" ] || [ "$item" = "detach" ]; then
        echo -e "\033[1;33m[Detach Session]\033[0m"
        echo ""
        echo "Cleanly leaves the active session."
        echo "The session continues running in the background."
        exit 0
    fi

    echo -e "\033[1;36m[Session: $item]\033[0m"
    echo -e "\033[90m--------------------------------------------------\033[0m"
    ttyman read --ansi -s "$item" 2>/dev/null || echo "(Session starting or inactive)"
    exit 0
fi

# 2. Kill helper: terminates session process by PID
if [ "${1:-}" = "--kill" ]; then
    raw_item="${2:-}"
    name=$(echo "$raw_item" | sed -E 's/^[ *][ ]//')
    if [ "$name" != "[detach]" ] && [ "$name" != "detach" ] && [ -n "$name" ]; then
        pid=$(ttyman list --json 2>/dev/null | jq -r --arg n "$name" '.[] | select(.name == $n) | .pid' 2>/dev/null || true)
        if [ -n "$pid" ] && [ "$pid" != "null" ]; then
            kill "$pid" 2>/dev/null || true
        fi
    fi
    exit 0
fi

# 3. Rename helper: interactively prompts for new session name
if [ "${1:-}" = "--rename" ]; then
    raw_item="${2:-}"
    name=$(echo "$raw_item" | sed -E 's/^[ *][ ]//')
    if [ "$name" != "[detach]" ] && [ "$name" != "detach" ] && [ -n "$name" ]; then
        echo ""
        read -r -p "Rename session '$name' to: " new_name < /dev/tty || true
        if [ -n "$new_name" ] && [ "$new_name" != "$name" ]; then
            if ! ttyman rename -s "$name" "$new_name" 2>&1; then
                echo "Failed to rename session."
                read -r -p "Press Enter to continue..." < /dev/tty || true
            fi
        fi
    fi
    exit 0
fi

# Helper: list items in MRU order (previous first, current at bottom before [detach])
list_items() {
    current="${TTYMAN_SESSION:-}"
    all_sessions=$(ttyman list --json 2>/dev/null | jq -r '.[].name' 2>/dev/null || true)
    seen=""

    # 1. List active sessions in MRU order from $TTYMAN_RECENT_SESSIONS (excluding current)
    for name in ${TTYMAN_RECENT_SESSIONS:-}; do
        if [ "$name" != "$current" ] && echo "$all_sessions" | grep -qx "$name"; then
            echo "  $name"
            seen="$seen $name "
        fi
    done

    # 2. List any remaining active sessions not yet in recent history (excluding current)
    echo "$all_sessions" | while IFS= read -r name; do
        if [ -n "$name" ] && [ "$name" != "$current" ] && [[ "$seen" != *" $name "* ]]; then
            echo "  $name"
        fi
    done

    # 3. Current session (at bottom, marked with *, can be selected or killed with Ctrl-X)
    if [ -n "$current" ] && echo "$all_sessions" | grep -qx "$current"; then
        echo "* $current"
    fi

    # 4. [detach] at the very end
    echo "  [detach]"
}

# 3. List helper: invoked by fzf reload
if [ "${1:-}" = "--list" ]; then
    list_items
    exit 0
fi

SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
current_sess="${TTYMAN_SESSION:-}"
prompt_label="ttyman"
if [ -n "$current_sess" ]; then
    prompt_label="ttyman [$current_sess]"
fi

# 4. Run FZF interactive selector
selected_raw=$(list_items | fzf \
    --prompt="${prompt_label} > " \
    --header="Enter: Switch/Create | Ctrl-R: Rename | Ctrl-X: Kill session | ESC: Cancel" \
    --print-query \
    --preview="\"$SCRIPT_PATH\" --preview {} {q}" \
    --preview-window="right:60%:wrap" \
    --bind="ctrl-r:execute(\"$SCRIPT_PATH\" --rename {})+reload(\"$SCRIPT_PATH\" --list)" \
    --bind="ctrl-x:execute(\"$SCRIPT_PATH\" --kill {})+reload(\"$SCRIPT_PATH\" --list)" || true)

if [ -z "$selected_raw" ]; then
    exit 130
fi

query=$(echo "$selected_raw" | sed -n '1p')
raw_selection=$(echo "$selected_raw" | sed -n '2p')
selection=$(echo "$raw_selection" | sed -E 's/^[ *][ ]//')

if [ "$selection" = "[detach]" ] || [ "$selection" = "detach" ]; then
    echo "detach"
elif [ -n "$selection" ]; then
    echo "attach:$selection"
elif [ -n "$query" ]; then
    echo "attach:$query"
else
    exit 130
fi
