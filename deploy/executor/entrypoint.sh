#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/guard.sh"
source "$(dirname "${BASH_SOURCE[0]}")/companions.sh"
source "$(dirname "${BASH_SOURCE[0]}")/repositories.sh"

: "${GIT_REMOTE:?}" "${BASE_BRANCH:?}" "${BRANCH:?}" "${MODEL:?}" "${MAX_TURNS:?}" "${PROMPT:?}"
: "${MATOS_TASK_TOKEN:?}"
harness="${MATOS_HARNESS:-claude-code}"
case "$harness" in
  claude-code) ;;
  codex) : "${MATOS_OPENAI_BASE_URL:?}" "${CODEX_API_KEY:?}" "${CODEX_HOME:?}" ;;
  antigravity) : "${GOOGLE_GEMINI_BASE_URL:?}" "${GEMINI_API_KEY:?}" ;;
  mistral-vibe) : "${MATOS_MISTRAL_BASE_URL:?}" "${MISTRAL_API_KEY:?}" "${VIBE_HOME:?}" ;;
  *)
    echo "unknown MATOS_HARNESS: $harness" >&2
    exit 2
    ;;
esac
mode="${MATOS_MODE:-change}"
case "$mode" in
  change | audit) ;;
  *)
    echo "unknown MATOS_MODE: $mode" >&2
    exit 2
    ;;
esac

askpass="$(mktemp /tmp/askpass.XXXXXX)"
trap 'rm -f "$askpass"' EXIT
cat >"$askpass" <<'ASKPASS'
#!/usr/bin/env bash
case "$1" in
  Username*) printf '%s\n' "x-access-token" ;;
  *) printf '%s\n' "$MATOS_TASK_TOKEN" ;;
esac
ASKPASS
chmod 0700 "$askpass"
export GIT_ASKPASS="$askpass"
export GIT_TERMINAL_PROMPT=0

git config --global user.name "${COMMIT_AUTHOR_NAME:-executor}"
git config --global user.email "${COMMIT_AUTHOR_EMAIL:-executor@localhost}"

git clone --depth 50 --branch "$BASE_BRANCH" "$GIT_REMOTE" repo >&2
marker cloned
extras_clone || exit 1

export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$COMPANION_WORKSPACE/.matos/target}"
companions_clone
write_registry_config

write_codex_config() {
  mkdir -p "$CODEX_HOME"
  cat >"$CODEX_HOME/config.toml" <<CODEX_CONFIG
model_provider = "matos"
web_search = "disabled"
check_for_update_on_startup = false

[model_providers.matos]
name = "matos"
base_url = "$MATOS_OPENAI_BASE_URL"
env_key = "CODEX_API_KEY"
wire_api = "responses"

[features]
image_generation = false
api_key_model_discovery = false

[analytics]
enabled = false
CODEX_CONFIG
}

write_antigravity_config() {
  mkdir -p "$HOME/.gemini/antigravity-cli"
  printf '%s\n' '{"modelProvider":"gemini"}' >"$HOME/.gemini/antigravity-cli/settings.json"
}

write_vibe_config() {
  mkdir -p "$VIBE_HOME"
  cat >"$VIBE_HOME/config.toml" <<VIBE_CONFIG
active_model = "matos"
enable_telemetry = false
enable_update_checks = false
enable_auto_update = false
enable_mcp = false
enable_connectors = false
enable_notifications = false
include_commit_signature = false
vibe_base_url = "$MATOS_MISTRAL_BASE_URL"
console_base_url = "$MATOS_MISTRAL_BASE_URL"

[experiments]
enable = false

[session_logging]
enabled = false
generate_titles = false

[[providers]]
name = "matos"
api_base = "$MATOS_MISTRAL_BASE_URL/v1"
api_key_env_var = "MISTRAL_API_KEY"
backend = "generic"
api_style = "openai"

[[models]]
name = "$MODEL"
provider = "matos"
alias = "matos"
max_context_length = 262144
VIBE_CONFIG
}

run_vibe() {
  local prompt_file output_dir status=0
  prompt_file="$(mktemp /tmp/prompt.XXXXXX)"
  output_dir="$(mktemp -d /tmp/vibe-out.XXXXXX)"
  printf '%s' "$PROMPT" >"$prompt_file"
  vibe --prompt-file "$prompt_file" --output streaming --output-dir "$output_dir" \
    --auto-approve --model matos --max-turns "$MAX_TURNS" || status=$?
  if [ -s "$output_dir/export.json" ]; then
    printf '{"type":"vibe_export","export":%s}\n' "$(tr -d '\n' <"$output_dir/export.json")"
  fi
  rm -rf "$prompt_file" "$output_dir"
  return "$status"
}

run_agent() {
  local run_mode="$1"
  case "$harness" in
    claude-code)
      if [ "$run_mode" = audit ]; then
        claude -p "$PROMPT" \
          --output-format stream-json \
          --verbose \
          --model "$MODEL" \
          --max-turns "$MAX_TURNS" \
          --allowedTools "Read,Glob,Grep,Bash,Write" \
          --disallowedTools "Edit,MultiEdit,NotebookEdit"
      else
        claude -p "$PROMPT" \
          --output-format stream-json \
          --verbose \
          --model "$MODEL" \
          --max-turns "$MAX_TURNS" \
          --allowedTools "Read,Edit,Write,Glob,Grep,Bash"
      fi
      ;;
    codex)
      codex exec --json --skip-git-repo-check --ephemeral \
        --dangerously-bypass-approvals-and-sandbox -m "$MODEL" -- "$PROMPT"
      ;;
    antigravity)
      export AGY_CLI_DISABLE_AUTO_UPDATE=true
      agy -p "$PROMPT" --output-format stream-json \
        --dangerously-skip-permissions --model "$MODEL"
      ;;
    mistral-vibe)
      run_vibe
      ;;
  esac
}

cd repo
case "$harness" in
  claude-code) export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 ;;
  codex) write_codex_config ;;
  antigravity) write_antigravity_config ;;
  mistral-vibe) write_vibe_config ;;
esac
agent_status=0

if [ "$mode" = audit ]; then
  : "${REPORT_PATH:?}" "${PLAN_PATH:?}"
  mkdir -p "$(dirname "$REPORT_PATH")" "$(dirname "$PLAN_PATH")"
  rm -f "$REPORT_PATH" "$PLAN_PATH"
  # Read-only: no Edit, and nothing is ever committed or pushed (the proxy
  # refuses a push of this run anyway). Write is kept for the report only.
  run_agent audit || agent_status=$?
  echo "$harness exited with $agent_status" >&2
  emit_report "$REPORT_PATH" || echo "no report at $REPORT_PATH" >&2
  # A plan is optional: the agent writes one only when the card asks for a change.
  emit_report "$PLAN_PATH" plan || true
  # The worker judges the audit from the report and the summary it read;
  # a stop of Claude Code is in its stream.
  marker audit_done
  exit 0
fi

: "${COMMIT_FALLBACK_MESSAGE:?}" "${CONTRIBUTION_PATH:?}"
: "${GUARD_MAX_FILES:?}" "${GUARD_MAX_LINES:?}"
mkdir -p "$(dirname "$CONTRIBUTION_PATH")"
rm -rf "$CONTRIBUTION_PATH" "$(dirname "$CONTRIBUTION_PATH")/contribution"
mkdir -p "$(dirname "$CONTRIBUTION_PATH")/contribution"
base_sha="$(git rev-parse HEAD)"
git switch -c "$BRANCH" >&2
run_agent change || agent_status=$?

# Commits the agent made itself are folded into one per repository, with its
# message; nothing is pushed unless every changed repository passes the guard.
publish_changes "$base_sha" || exit $?

exit "$agent_status"
