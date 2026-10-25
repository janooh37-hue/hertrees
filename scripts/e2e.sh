#!/usr/bin/env bash
# Live Herdr/Claude integration test. Uses the existing binary; never builds it.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: bash scripts/e2e.sh [--preset blank|plan] [--keep]

Creates a disposable main-branch shell workspace, drives Hertrees in a test tab
in the caller's workspace, and checks the new worktree, tree grouping, and Claude.
Requires a running Herdr pane, git, jq, python3, and an authenticated Claude CLI.
The plan preset submits a real planning request to Claude (normal API usage).

Defaults: E2E_PRESET=blank, E2E_TIMEOUT=90 (seconds per wait).
--keep retains the workspaces, tab, agent, checkout, and temporary config even
on failure; printed IDs/paths can be used for inspection and manual cleanup.
Without --keep, an EXIT trap cleans up on success, failure, Ctrl-C, or termination.
EOF
}

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$*"; }
preset=${E2E_PRESET:-blank}
timeout=${E2E_TIMEOUT:-90}
keep=0
while (($#)); do
    case $1 in
        --preset) (($# >= 2)) || fail '--preset needs blank or plan'; preset=$2; shift 2 ;;
        --keep) keep=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; fail "unknown option: $1" ;;
    esac
done
case $preset in blank|plan) ;; *) fail 'preset must be blank or plan' ;; esac
[[ $timeout =~ ^[1-9][0-9]*$ ]] || fail 'E2E_TIMEOUT must be a positive integer'
[[ ${HERDR_ENV:-} == 1 ]] || fail 'run this script inside Herdr (HERDR_ENV=1)'
project_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
binary=$project_dir/bin/hertrees
[[ -x $binary ]] || fail "missing executable $binary; build it first"
for dependency in herdr git jq python3 claude; do
    command -v "$dependency" >/dev/null || fail "missing dependency: $dependency"
done
original_workspace=${HERDR_WORKSPACE_ID:?Herdr must provide HERDR_WORKSPACE_ID}
original_tab=${HERDR_TAB_ID:?Herdr must provide HERDR_TAB_ID}
original_pane=${HERDR_PANE_ID:?Herdr must provide HERDR_PANE_ID}
root=
repo=
parent_workspace=
test_tab=
picker_pane=
worktree_workspace=
worktree_path=
claude_pane=
branch=

# The pane.focus API accepts an exact pane ID; the installed CLI exposes only
# directional pane focus. Do not guess a neighbor or require the caller to be an agent.
restore_focus() {
    herdr workspace focus "$original_workspace" >/dev/null || return 1
    herdr tab focus "$original_tab" >/dev/null || return 1
    python3 - "$original_pane" <<'PY'
import json
import os
import socket
import sys

config = os.path.join(os.environ.get('XDG_CONFIG_HOME') or os.path.expanduser('~/.config'), 'herdr')
session = os.environ.get('HERDR_SESSION', '')
default = os.path.join(config, 'sessions', session, 'herdr.sock') if session and session != 'default' else os.path.join(config, 'herdr.sock')
path = os.environ.get('HERDR_SOCKET_PATH') or default
with socket.socket(socket.AF_UNIX) as connection:
    connection.settimeout(5)
    connection.connect(path)
    request = {'id': 'hertrees-e2e-focus', 'method': 'pane.focus', 'params': {'pane_id': sys.argv[1]}}
    connection.sendall((json.dumps(request) + '\n').encode())
    response = json.loads(connection.makefile().readline())
    if 'error' in response:
        raise RuntimeError(response['error'])
PY
}

cleanup() {
    status=$?
    trap - EXIT INT TERM
    set +e
    if ((status)); then
        printf 'FAIL: e2e exited with status %s\n' "$status" >&2
        if [[ -n $picker_pane ]]; then
            printf '\n--- Hertrees visible output ---\n' >&2
            herdr pane read "$picker_pane" --source visible --lines 80 >&2
        fi
        if [[ -n $claude_pane ]]; then
            printf '\n--- Claude visible output ---\n' >&2
            herdr pane read "$claude_pane" --source visible --lines 100 >&2
        fi
    fi
    cleanup_failed=0
    if ((keep)); then
        printf 'KEEP: root=%q repo=%q branch=%q\n' "$root" "$repo" "$branch"
        printf 'KEEP: parent_workspace=%q test_tab=%q worktree_workspace=%q claude_pane=%q checkout=%q\n' \
            "$parent_workspace" "$test_tab" "$worktree_workspace" "$claude_pane" "$worktree_path"
        printf 'Cleanup: herdr worktree remove --workspace <worktree_workspace> --force; herdr workspace close <parent_workspace>; herdr tab close <test_tab>; rm -rf -- <root>\n'
    else
        # Discover by our unique repo as well as saved IDs: launch may create a
        # checkout and then fail before the script has received its workspace ID.
        workspaces=$(herdr workspace list)
        if [[ -n $repo ]]; then
            linked=$(printf '%s' "$workspaces" | jq -r --arg repo "$repo" \
                '.result.workspaces[] | select(.worktree.repo_root == $repo and .worktree.is_linked_worktree == true) | .workspace_id')
            for workspace in $linked; do
                # Removing/closing its terminal runtime also terminates Claude,
                # including a Claude still blocked at its startup trust dialog.
                if ! herdr worktree remove --workspace "$workspace" --force >/dev/null; then
                    printf 'FAIL: could not remove worktree workspace %s\n' "$workspace" >&2
                    cleanup_failed=1
                    herdr workspace close "$workspace" >/dev/null
                fi
            done
        fi
        if [[ -n $parent_workspace ]]; then
            herdr workspace close "$parent_workspace" >/dev/null || cleanup_failed=1
        fi
        if [[ -n $test_tab ]]; then
            herdr tab close "$test_tab" >/dev/null || cleanup_failed=1
        fi
        if [[ -n $repo ]]; then
            remaining=$(herdr workspace list) || cleanup_failed=1
            printf '%s' "$remaining" | jq -e --arg repo "$repo" \
                '[.result.workspaces[] | select(.worktree.repo_root == $repo)] | length == 0' >/dev/null || cleanup_failed=1
            agents=$(herdr agent list) || cleanup_failed=1
            printf '%s' "$agents" | jq -e --arg repo "$repo" --arg workspace "$worktree_workspace" --arg pane "$claude_pane" \
                '[.result.agents[] | select((.workspace_id == $workspace and $workspace != "") or (.pane_id == $pane and $pane != "") or .cwd == $repo or .foreground_cwd == $repo)] | length == 0' >/dev/null || cleanup_failed=1
            if [[ -n $worktree_path && -e $worktree_path ]]; then
                printf 'FAIL: checkout remains: %s\n' "$worktree_path" >&2
                cleanup_failed=1
            fi
        fi
        if [[ -n $test_tab ]]; then
            tabs=$(herdr tab list --workspace "$original_workspace") || cleanup_failed=1
            printf '%s' "$tabs" | jq -e --arg tab "$test_tab" \
                '[.result.tabs[] | select(.tab_id == $tab)] | length == 0' >/dev/null || cleanup_failed=1
        fi
        if ((cleanup_failed == 0)); then
            [[ -z $root ]] || rm -rf -- "$root" || cleanup_failed=1
        else
            printf 'FAIL: cleanup incomplete; retained temporary files at %s\n' "$root" >&2
        fi
        if ((cleanup_failed == 0)); then
            pass 'cleanup: test tab, workspaces, checkout, Claude agent, and temporary directory removed'
        fi
    fi
    restore_focus || { printf 'FAIL: could not restore caller focus\n' >&2; cleanup_failed=1; }
    ((cleanup_failed == 0)) || status=1
    if ((status == 0)); then
        pass "Hertrees e2e ($preset)"
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

wait_for() {
    local description=$1 deadline=$((SECONDS + timeout))
    shift
    until "$@"; do
        ((SECONDS < deadline)) || fail "timed out after ${timeout}s: $description"
        sleep 0.25
    done
}

picker_contains() {
    local output
    output=$(herdr pane read "$picker_pane" --source visible --lines 100) || return 1
    [[ $output == *"$1"* ]]
}

find_worktree() {
    local json
    json=$(herdr worktree list --cwd "$repo") || return 1
    worktree_workspace=$(printf '%s' "$json" | jq -r --arg branch "$branch" \
        '.result.worktrees[] | select(.branch == $branch) | .open_workspace_id // empty') || return 1
    worktree_path=$(printf '%s' "$json" | jq -r --arg branch "$branch" \
        '.result.worktrees[] | select(.branch == $branch) | .path') || return 1
    [[ -n $worktree_workspace && -d $worktree_path ]]
}

find_claude() {
    local json
    json=$(herdr agent list) || return 1
    claude_pane=$(printf '%s' "$json" | jq -r --arg workspace "$worktree_workspace" \
        '.result.agents[] | select(.workspace_id == $workspace and .agent == "claude") | .pane_id') || return 1
    [[ -n $claude_pane ]]
}

claude_ready() {
    local json status output
    json=$(herdr agent list) || return 1
    status=$(printf '%s' "$json" | jq -r --arg pane "$claude_pane" \
        '.result.agents[] | select(.pane_id == $pane and .agent == "claude") | .agent_status') || return 1
    output=$(herdr pane read "$claude_pane" --source visible --lines 100) || return 1
    if [[ $output == *'No, exit'* && $output == *'trust'* ]]; then
        if ((trust_answered == 0)); then
            herdr pane send-keys "$claude_pane" down enter >/dev/null || return 1
            trust_answered=1
            printf 'INFO: accepted Claude folder trust (down, enter)\n'
        fi
        return 1
    fi
    [[ $status == idle || $status == done || $status == working ]]
}

plan_received() {
    local json output status
    json=$(herdr agent list) || return 1
    status=$(printf '%s' "$json" | jq -r --arg pane "$claude_pane" \
        '.result.agents[] | select(.pane_id == $pane and .agent == "claude") | .agent_status') || return 1
    output=$(herdr pane read "$claude_pane" --source recent-unwrapped --lines 200) || return 1
    [[ $output == *'PLAN.md'* && ( $status == working || $status == done || $status == idle || $status == blocked ) ]]
}

root=$(mktemp -d /tmp/hertrees-e2e.XXXXXXXX)
repo=$root/repo
branch=hertrees-e2e-${root##*.}
mkdir "$repo" "$root/config"
git -C "$repo" init -q --initial-branch=main
printf 'Hertrees disposable integration-test repository.\n' >"$repo/README.txt"
git -C "$repo" add README.txt
git -C "$repo" -c user.name='Hertrees E2E' -c user.email='hertrees-e2e@example.invalid' commit -qm 'Initial test commit'
printf 'INFO: repo=%s branch=%s preset=%s\n' "$repo" "$branch" "$preset"

created=$(herdr workspace create --cwd "$repo" --label hertrees-test-main --no-focus)
parent_workspace=$(printf '%s' "$created" | jq -er '.result.workspace.workspace_id')
created=$(herdr tab create --workspace "$original_workspace" --cwd "$repo" --label hertrees-test --no-focus)
test_tab=$(printf '%s' "$created" | jq -er '.result.tab.tab_id')
picker_pane=$(printf '%s' "$created" | jq -er '.result.root_pane.pane_id')
context=$(jq -cn --arg repo "$repo" '{focused_pane_cwd:$repo}')
printf -v launch 'env HERDR_PLUGIN_ID=hertrees HERDR_PLUGIN_CONFIG_DIR=%q HERDR_PLUGIN_CONTEXT_JSON=%q %q' \
    "$root/config" "$context" "$binary"
herdr pane run "$picker_pane" "$launch" >/dev/null
wait_for 'Hertrees alternate-screen picker' picker_contains '^x new'
wait_for 'loaded main worktree' picker_contains 'main'
herdr pane send-keys "$picker_pane" ctrl+x >/dev/null
wait_for 'new branch editor' picker_contains 'new worktree'
herdr pane send-text "$picker_pane" "$branch" >/dev/null
herdr pane send-keys "$picker_pane" enter >/dev/null
wait_for 'agent picker' picker_contains 'blank'
herdr pane send-text "$picker_pane" claude >/dev/null
if [[ $preset == plan ]]; then
    herdr pane send-keys "$picker_pane" tab >/dev/null
    wait_for 'plan preset' picker_contains 'plan'
fi
herdr pane send-keys "$picker_pane" enter >/dev/null
wait_for 'new branch worktree' find_worktree
[[ $(git -C "$worktree_path" branch --show-current) == "$branch" ]] || fail 'checkout has the wrong branch'
pass "branch worktree exists: $worktree_path ($worktree_workspace)"

workspaces=$(herdr workspace list)
printf '%s' "$workspaces" | jq -e --arg parent "$parent_workspace" --arg child "$worktree_workspace" --arg repo "$repo" '
    .result.workspaces as $all |
    ($all[] | select(.workspace_id == $parent)) as $parent |
    ($all[] | select(.workspace_id == $child)) as $child |
    $parent.worktree.repo_root == $repo and
    $parent.worktree.is_linked_worktree == false and
    $child.worktree.repo_root == $repo and
    $child.worktree.is_linked_worktree == true and
    $parent.worktree.repo_key == $child.worktree.repo_key' >/dev/null || fail 'worktree workspace is not grouped with the main shell workspace'
pass "tree grouping: $worktree_workspace shares repo_key with parent $parent_workspace"
wait_for 'Claude agent in branch workspace' find_claude
trust_answered=0
wait_for 'Claude startup and folder trust' claude_ready
herdr agent list | jq -e --arg pane "$claude_pane" --arg checkout "$worktree_path" \
    '.result.agents[] | select(.pane_id == $pane and .agent == "claude") | (.foreground_cwd // .cwd) == $checkout' >/dev/null || fail 'Claude is not running in the worktree checkout'
pass "Claude runs in the branch checkout: $claude_pane"
if [[ $preset == plan ]]; then
    wait_for 'plan prompt delivery (PLAN.md in Claude transcript)' plan_received
    pass 'plan preset prompt reached Claude (PLAN.md visible in its transcript)'
fi
