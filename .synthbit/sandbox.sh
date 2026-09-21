#!/usr/bin/env bash
# =============================================================================
# sandbox.sh — Managed Git worktree helper
#
# Provides isolated worktree-based runs with durable history guarantees.
# Each run receives a unique branch and worktree path derived from a run ID.
#
# Commands:
#   create              Allocate a new run (branch + worktree); prints run-id
#   status  <run-id>    Show run state (branch, worktree, recovery, ledger)
#   promote <run-id>    Fast-forward merge run into current branch, then clean up
#   abort   <run-id>    Preserve run history in recovery ref, remove worktree
#
# Options:
#   --dry-run           Show actions without executing
#   --force             Abort: force-remove worktree and delete branch (data loss)
# =============================================================================
set -eu

# Configurable state
DRY_RUN=0
FORCE=0

# --- Output helpers ---
_info()  { printf '[INFO]  %s\n' "$*" >&2; }
_warn()  { printf '[WARN]  %s\n' "$*" >&2; }
_error() { printf '[ERROR] %s\n' "$*" >&2; }

usage() {
  cat <<'EOF'
Usage: sandbox.sh [--dry-run] [--force] <command> [run-id]

Commands:
  create              Allocate a new run; prints run-id
  status  <run-id>    Show run state
  promote <run-id>    Fast-forward merge run into current branch, then clean up
  abort   <run-id>    Preserve run history in recovery ref, remove worktree

Options:
  --dry-run    Show actions without executing
  --force      Abort: force-remove worktree and delete branch (data loss)
EOF
}

# --- Path helpers ---
_repo_root() {
  git rev-parse --show-toplevel
}

_gen_run_id() {
  if [[ -r /proc/sys/kernel/random/uuid ]]; then
    tr '[:upper:]' '[:lower:]' < /proc/sys/kernel/random/uuid
  elif command -v uuidgen >/dev/null 2>&1; then
    uuidgen | tr '[:upper:]' '[:lower:]'
  else
    od -A n -t x -N 16 /dev/urandom | tr -d ' \n'
  fi
}

_branch_name() {
  printf 'synthbit/run/%s' "$1"
}

_worktree_path() {
  printf '%s/.synthbit/worktrees/%s' "$(_repo_root)" "$1"
}

_recovery_ref() {
  printf 'refs/synthbit/recovery/%s' "$1"
}

_ledger_path() {
  local git_dir
  git_dir="$(git -C "$(_repo_root)" rev-parse --git-dir)"
  case "$git_dir" in
    /*) printf '%s/synthbit/ledger.jsonl' "$git_dir" ;;
    *)  printf '%s/%s/synthbit/ledger.jsonl' "$(_repo_root)" "$git_dir" ;;
  esac
}

# Append event to authoritative ledger
_log_event() {
  local run_id="$1" op="$2"
  local ledger ts
  ledger="$(_ledger_path)"
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  mkdir -p "$(dirname "$ledger")"
  printf '{"ts":"%s","run_id":"%s","op":"%s"}\n' "$ts" "$run_id" "$op" >> "$ledger"
}

# Run a command, honoring dry-run
_run() {
  if (( DRY_RUN )); then
    printf '[DRY-RUN] %s\n' "$*" >&2
  else
    "$@"
  fi
}

# --- Validation ---

# Verify run ownership: branch and ledger entry must exist
_verify_ownership() {
  local run_id="$1"
  local branch ledger
  branch="$(_branch_name "$run_id")"
  ledger="$(_ledger_path)"

  if ! git -C "$MAIN_ROOT" rev-parse --verify "$branch" >/dev/null 2>&1; then
    _error "No managed branch for run: $run_id"
    return 1
  fi

  if [[ ! -f "$ledger" ]] || ! grep -qF "\"run_id\":\"$run_id\"" "$ledger" 2>/dev/null; then
    _error "No ledger entry for run: $run_id"
    return 1
  fi

  return 0
}

# Verify all run commits are reachable from a durable ref in the
# authoritative store (recovery ref, merged HEAD, or remote tracking branch).
_verify_history_durable() {
  local run_id="$1"
  local branch ref run_commit
  branch="$(_branch_name "$run_id")"
  ref="$(_recovery_ref "$run_id")"
  run_commit="$(git -C "$MAIN_ROOT" rev-parse "$branch")"

  # Recovery ref exists => history preserved
  if git -C "$MAIN_ROOT" rev-parse --verify "$ref" >/dev/null 2>&1; then
    _info "History durable via recovery ref: $ref"
    return 0
  fi

  # Run commit is ancestor of HEAD => merged, durable
  if git -C "$MAIN_ROOT" merge-base --is-ancestor "$run_commit" HEAD 2>/dev/null; then
    _info "History durable: merged into current HEAD"
    return 0
  fi

  # Run commit reachable from any remote tracking branch
  local remote_ref
  for remote_ref in $(git -C "$MAIN_ROOT" for-each-ref --format='%(refname)' refs/remotes/ 2>/dev/null); do
    if git -C "$MAIN_ROOT" merge-base --is-ancestor "$run_commit" "$remote_ref" 2>/dev/null; then
      _info "History durable: published to $remote_ref"
      return 0
    fi
  done

  _error "History NOT durable: commit ${run_commit:0:8} unreachable from any durable ref"
  return 1
}

# --- Commands ---

cmd_create() {
  local run_id branch_name wt_path parent_commit
  run_id="$(_gen_run_id)"
  branch_name="$(_branch_name "$run_id")"
  wt_path="$(_worktree_path "$run_id")"
  parent_commit="$(git -C "$MAIN_ROOT" rev-parse HEAD)"

  if (( DRY_RUN )); then
    printf '[DRY-RUN] git worktree add -b %s %s %s\n' "$branch_name" "$wt_path" "$parent_commit" >&2
    printf '%s\n' "$run_id"
    return 0
  fi

  # Create worktree (creates branch from HEAD of main repo)
  git -C "$MAIN_ROOT" worktree add -b "$branch_name" "$wt_path" "$parent_commit" >/dev/null

  # Register in ledger
  _log_event "$run_id" "create"

  # Ensure worktree parent directory is gitignored in main repo
  local ignore_file ignore_entry
  ignore_file="$MAIN_ROOT/.gitignore"
  ignore_entry='/.synthbit/worktrees/'
  if [[ -f "$ignore_file" ]] && grep -qF "$ignore_entry" "$ignore_file" 2>/dev/null; then
    :
  else
    printf '%s\n' "$ignore_entry" >> "$ignore_file"
  fi

  printf '%s\n' "$run_id"
}

cmd_status() {
  local run_id="$1"
  local branch_name wt_path ledger ref
  branch_name="$(_branch_name "$run_id")"
  wt_path="$(_worktree_path "$run_id")"
  ledger="$(_ledger_path)"
  ref="$(_recovery_ref "$run_id")"

  echo "Run ID:    $run_id"
  echo "Branch:    $branch_name"
  echo "Path:      $wt_path"

  if git -C "$MAIN_ROOT" rev-parse --verify "$branch_name" >/dev/null 2>&1; then
    local commit msg
    commit="$(git -C "$MAIN_ROOT" rev-parse --short "$branch_name")"
    msg="$(git -C "$MAIN_ROOT" log -1 --format='%s' "$branch_name" 2>/dev/null || true)"
    echo "Head:      $commit"
    echo "Message:   ${msg:-(empty)}"
  else
    echo "Head:      (branch missing)"
  fi

  if [[ -d "$wt_path" ]]; then
    local wt_branch wt_commit
    wt_branch="$(git -C "$wt_path" symbolic-ref --short HEAD 2>/dev/null || echo '(detached)')"
    wt_commit="$(git -C "$wt_path" rev-parse --short HEAD 2>/dev/null || echo '(unknown)')"
    echo "Worktree:  present ($wt_branch @ $wt_commit)"
  else
    echo "Worktree:  absent"
  fi

  if git -C "$MAIN_ROOT" rev-parse --verify "$ref" >/dev/null 2>&1; then
    echo "Recovery:  $(git -C "$MAIN_ROOT" rev-parse --short "$ref")"
  else
    echo "Recovery:  absent"
  fi

  if [[ -f "$ledger" ]] && grep -qF "\"run_id\":\"$run_id\"" "$ledger" 2>/dev/null; then
    echo "Ledger:    registered"
  else
    echo "Ledger:    not found"
  fi

  if git -C "$MAIN_ROOT" rev-parse --verify "$branch_name" >/dev/null 2>&1; then
    local base ahead behind
    base="$(git -C "$MAIN_ROOT" merge-base HEAD "$branch_name" 2>/dev/null)" || true
    if [[ -n "$base" ]]; then
      ahead="$(git -C "$MAIN_ROOT" rev-list --count "$base..$branch_name" 2>/dev/null || echo '?')"
      behind="$(git -C "$MAIN_ROOT" rev-list --count "$base..HEAD" 2>/dev/null || echo '?')"
      echo "Distance:  +$ahead/-$behind from HEAD"
    fi
  fi
}

cmd_promote() {
  local run_id="$1"
  local branch_name wt_path

  _verify_ownership "$run_id" || return 1

  branch_name="$(_branch_name "$run_id")"
  wt_path="$(_worktree_path "$run_id")"

  _info "Promoting run $run_id via fast-forward merge"

  # Fast-forward merge into main repo's current branch
  _run git -C "$MAIN_ROOT" merge --ff-only "$branch_name"

  # Verify history is durable before removing worktree
  if ! (( DRY_RUN )); then
    _verify_history_durable "$run_id" || {
      _error "History durability verification failed"
      return 1
    }
  fi

  # Remove worktree (idempotent)
  if [[ -d "$wt_path" ]]; then
    _info "Removing worktree: $wt_path"
    _run git -C "$MAIN_ROOT" worktree remove "$wt_path"
  else
    _info "Worktree already absent"
  fi

  # Delete branch (safe: fully merged after ff-only)
  if git -C "$MAIN_ROOT" rev-parse --verify "$branch_name" >/dev/null 2>&1; then
    _info "Deleting branch: $branch_name"
    _run git -C "$MAIN_ROOT" branch -d "$branch_name"
  fi

  # Clean up recovery ref if present
  local ref
  ref="$(_recovery_ref "$run_id")"
  if git -C "$MAIN_ROOT" rev-parse --verify "$ref" >/dev/null 2>&1; then
    _info "Removing recovery ref: $ref"
    _run git -C "$MAIN_ROOT" update-ref -d "$ref"
  fi

  _log_event "$run_id" "promote"
  _info "Run $run_id promoted successfully"
}

cmd_abort() {
  local run_id="$1"
  local branch_name wt_path ref run_commit

  _verify_ownership "$run_id" || return 1

  branch_name="$(_branch_name "$run_id")"
  wt_path="$(_worktree_path "$run_id")"
  ref="$(_recovery_ref "$run_id")"
  run_commit="$(git -C "$MAIN_ROOT" rev-parse "$branch_name")"

  _info "Aborting run $run_id"

  # Preserve committed history in recovery ref (always)
  if ! git -C "$MAIN_ROOT" rev-parse --verify "$ref" >/dev/null 2>&1; then
    _info "Creating recovery ref: $ref -> ${run_commit:0:8}"
    if ! (( DRY_RUN )); then
      git -C "$MAIN_ROOT" update-ref "$ref" "$run_commit"
    fi
  else
    _info "Recovery ref already exists: $ref"
  fi

  # Remove worktree
  if [[ -d "$wt_path" ]]; then
    if (( FORCE )); then
      _warn "Force-removing worktree (uncommitted changes lost): $wt_path"
      _run git -C "$MAIN_ROOT" worktree remove --force "$wt_path"
    else
      _info "Removing worktree: $wt_path"
      _run git -C "$MAIN_ROOT" worktree remove "$wt_path"
    fi
  else
    _info "Worktree already absent"
  fi

  # Delete branch only with explicit --force (data loss)
  if git -C "$MAIN_ROOT" rev-parse --verify "$branch_name" >/dev/null 2>&1; then
    if (( FORCE )); then
      _warn "Deleting branch (data loss): $branch_name"
      _run git -C "$MAIN_ROOT" branch -D "$branch_name"
    else
      _info "Preserving branch: $branch_name"
    fi
  fi

  _log_event "$run_id" "abort"
  _info "Run $run_id aborted (history preserved in $ref)"
}

# --- Main ---
main() {
  local cmd="" run_id=""

  while (( $# > 0 )); do
    case "$1" in
      --dry-run) DRY_RUN=1; shift ;;
      --force)   FORCE=1; shift ;;
      --help|-h) usage; exit 0 ;;
      create|status|promote|abort)
        if [[ -n "$cmd" ]]; then
          _error "Multiple commands specified: $cmd, $1"
          usage; return 1
        fi
        cmd="$1"; shift
        ;;
      *)
        if [[ -z "$run_id" ]]; then
          run_id="$1"; shift
        else
          _error "Unknown argument: $1"
          usage; return 1
        fi
        ;;
    esac
  done

  if [[ -z "$cmd" ]]; then
    usage
    return 1
  fi

  # Must be in a git repository
  if ! MAIN_ROOT="$(_repo_root)" 2>/dev/null; then
    _error "Not in a git repository"
    return 1
  fi

  case "$cmd" in
    create)
      [[ -z "$run_id" ]] || { _error "create takes no run-id"; usage; return 1; }
      cmd_create
      ;;
    status)
      [[ -n "$run_id" ]] || { _error "status requires run-id"; usage; return 1; }
      cmd_status "$run_id"
      ;;
    promote)
      [[ -n "$run_id" ]] || { _error "promote requires run-id"; usage; return 1; }
      cmd_promote "$run_id"
      ;;
    abort)
      [[ -n "$run_id" ]] || { _error "abort requires run-id"; usage; return 1; }
      cmd_abort "$run_id"
      ;;
    *)
      _error "Unknown command: $cmd"
      usage
      return 1
      ;;
  esac
}

main "$@"
