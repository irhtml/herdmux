#!/usr/bin/env bash

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ -x "$PLUGIN_DIR/bin/tmux-agent-sidebar" ]]; then
    SIDEBAR_BINARY="$PLUGIN_DIR/bin/tmux-agent-sidebar"
elif [[ -x "$PLUGIN_DIR/target/release/tmux-agent-sidebar" ]]; then
    SIDEBAR_BINARY="$PLUGIN_DIR/target/release/tmux-agent-sidebar"
elif command -v "tmux-agent-sidebar" &>/dev/null; then
    SIDEBAR_BINARY="tmux-agent-sidebar"
fi

if [[ -z "$SIDEBAR_BINARY" ]]; then
    tmux run-shell -b "bash '$PLUGIN_DIR/install-wizard.sh'"
    exit 0
fi

INSTALLED_VERSION="$("$SIDEBAR_BINARY" version 2>/dev/null)"
EXPECTED_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$PLUGIN_DIR/Cargo.toml")"

if [[ -n "$EXPECTED_VERSION" && "$INSTALLED_VERSION" != "$EXPECTED_VERSION" ]]; then
    tmux run-shell -b "SIDEBAR_UPDATE=1 bash '$PLUGIN_DIR/install-wizard.sh'"
    exit 0
fi

tmux set -g @agent_sidebar_bin "$SIDEBAR_BINARY"

tmux source-file "$PLUGIN_DIR/agent-sidebar.conf"

# --- Resume agents after tmux-resurrect (opt-in: set -g @sidebar_resume on) ---
# Only claims a resurrect hook that is unset or already ours, so a user's
# own hook is never replaced.
if [[ "$(tmux show -gv @sidebar_resume 2>/dev/null)" == "on" ]]; then
    claim_resurrect_hook() {
        local name="$1" value="$2" current
        current="$(tmux show -gv "$name" 2>/dev/null)"
        if [[ -z "$current" || "$current" == *tmux-agent-sidebar*" resume "* ]]; then
            tmux set -g "$name" "$value"
        fi
    }
    quoted_bin="$(printf '%q' "$SIDEBAR_BINARY")"
    claim_resurrect_hook @resurrect-hook-post-save-layout "$quoted_bin resume save --quiet --resurrect-file"
    claim_resurrect_hook @resurrect-hook-post-restore-all "$quoted_bin resume restore --detach"
fi
