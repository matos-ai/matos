GUARD_TAB=$'\t'
GUARD_LINT_LINES=20
GUARD_LINT_LINE_CHARS=300
GUARD_LINT_TIMEOUT=900

guard_reasons=()
quality_status=not_run
quality_excerpt=""

guard_timeout() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$@"
  else
    shift
    "$@"
  fi
}

guard_json() {
  tr '\t' ' ' | LC_ALL=C tr -d '\000-\010\013-\037' | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' |
    awk 'NR > 1 { printf "\\n" } { printf "%s", $0 }'
}

guard_changed_files() {
  local file
  while IFS= read -r -d '' file; do
    printf '%s\n' "$file"
  done < <(git diff --cached --name-only --diff-filter=d --no-renames -z)
}

guard_denied_paths() {
  local path base entry allowed hit
  while IFS= read -r -d '' path; do
    base="${path##*/}"
    allowed=false
    while IFS= read -r entry; do
      if [ -n "$entry" ]; then
        case "$path" in "$entry"*) allowed=true ;; esac
      fi
    done <<<"${GUARD_ALLOW_PATHS:-}"
    if [ "$allowed" = true ]; then
      continue
    fi
    while IFS= read -r entry; do
      [ -n "$entry" ] || continue
      hit=false
      case "$entry" in
        */) case "$path" in "$entry"*) hit=true ;; esac ;;
        *) case "$base" in $entry) hit=true ;; esac ;;
      esac
      if [ "$hit" = true ]; then
        printf '%s\n' "$path"
        break
      fi
    done <<<"${GUARD_DENY_PATHS:-}"
  done < <(git diff --cached --name-only --no-renames -z)
}

guard_size() {
  git diff --cached --numstat --no-renames | awk '
    { files++; if ($1 ~ /^[0-9]+$/) lines += $1; if ($2 ~ /^[0-9]+$/) lines += $2 }
    END { printf "%d %d\n", files, lines }
  '
}

guard_scan() {
  local added="$1" label="$2" flags="$3" pattern="$4" file
  while IFS= read -r file; do
    guard_reasons+=("secret possible ($label) dans $file")
  done < <(grep -a"${flags}E" -- "^[^${GUARD_TAB}]*${GUARD_TAB}.*(${pattern})" "$added" | cut -f1 | sort -u)
}

guard_secrets() {
  local added
  added="$(mktemp /tmp/added.XXXXXX)"
  git diff --cached -U0 --no-color --no-renames | awk -v tab="$GUARD_TAB" '
    /^diff --git / { header = 1; next }
    /^@@/ { header = 0; next }
    header {
      if (substr($0, 1, 4) == "+++ ") {
        file = substr($0, 5)
        gsub(/^"|"$/, "", file)
        sub(/^b\//, "", file)
      }
      next
    }
    substr($0, 1, 1) == "+" { printf "%s%s%s\n", file, tab, substr($0, 2) }
  ' >"$added"
  guard_scan "$added" "cle privee" "" '-----BEGIN ([A-Z0-9]+ )*PRIVATE KEY-----'
  guard_scan "$added" "jeton GitHub" "" 'gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,}'
  guard_scan "$added" "cle d'acces AWS" "" 'AKIA[0-9A-Z]{16}'
  guard_scan "$added" "cle Anthropic" "" 'sk-ant-[A-Za-z0-9_-]{20,}'
  guard_scan "$added" "cle OpenAI" "" 'sk-proj-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9]{32,}'
  guard_scan "$added" "cle Google" "" 'AIza[0-9A-Za-z_-]{35}'
  guard_scan "$added" "jeton Slack" "" 'xox[baprs]-[0-9A-Za-z-]{10,}'
  guard_scan "$added" "mot de passe ou secret en dur" i \
    "[A-Za-z0-9_]*(password|passwd|secret)[A-Za-z0-9_]*[\"']?[[:space:]]*[=:][[:space:]]*[\"'][^\"']{3,}[\"']"
  rm -f "$added"
}

guard_check() {
  local path files lines
  guard_reasons=()
  while IFS= read -r path; do
    guard_reasons+=("chemin sensible : $path")
  done < <(guard_denied_paths)
  read -r files lines < <(guard_size)
  if [ "$files" -gt "${GUARD_MAX_FILES:?}" ]; then
    guard_reasons+=("trop de fichiers : $files (maximum $GUARD_MAX_FILES)")
  fi
  if [ "$lines" -gt "${GUARD_MAX_LINES:?}" ]; then
    guard_reasons+=("trop de lignes modifiees : $lines (maximum $GUARD_MAX_LINES)")
  fi
  guard_secrets
  [ "${#guard_reasons[@]}" -eq 0 ]
}

guard_blocked_marker() {
  local reason reasons=""
  for reason in "${guard_reasons[@]}"; do
    reasons="${reasons:+$reasons,}\"$(printf '%s' "$reason" | guard_json)\""
  done
  printf '{"type":"matos","event":"guard_blocked","reasons":[%s]}\n' "$reasons"
}

guard_node_files() {
  local file
  while IFS= read -r file; do
    printf './%s\n' "$file"
  done < <(guard_changed_files)
}

guard_run_on_files() {
  local tool="$1"
  shift
  local files=() file
  while IFS= read -r file; do
    files+=("$file")
  done < <(guard_node_files)
  [ "${#files[@]}" -gt 0 ] || return 0
  "$tool" "$@" "${files[@]}"
}

guard_format() {
  if [ -f Cargo.toml ] && command -v cargo >/dev/null 2>&1; then
    cargo fmt >&2 || echo "cargo fmt failed, its result is not applied" >&2
  fi
  if [ -x node_modules/.bin/biome ]; then
    guard_run_on_files node_modules/.bin/biome format --write --no-errors-on-unmatched \
      --files-ignore-unknown=true >&2 || echo "biome format failed" >&2
  elif [ -x node_modules/.bin/prettier ]; then
    guard_run_on_files node_modules/.bin/prettier --write --ignore-unknown >&2 ||
      echo "prettier failed" >&2
  fi
  git add -A
}

guard_lint() {
  local out status=0 ran=false
  quality_status=not_run
  quality_excerpt=""
  out="$(mktemp /tmp/lint.XXXXXX)"
  if [ -f Cargo.toml ] && command -v cargo >/dev/null 2>&1; then
    ran=true
    guard_timeout "$GUARD_LINT_TIMEOUT" cargo clippy --all-targets -- -D warnings >>"$out" 2>&1 || status=1
  fi
  if [ -x node_modules/.bin/biome ]; then
    ran=true
    guard_timeout "$GUARD_LINT_TIMEOUT" node_modules/.bin/biome check . >>"$out" 2>&1 || status=1
  elif [ -f package.json ] && [ -d node_modules ] && command -v npm >/dev/null 2>&1 &&
    grep -q '"lint"[[:space:]]*:' package.json; then
    ran=true
    guard_timeout "$GUARD_LINT_TIMEOUT" npm run --silent lint >>"$out" 2>&1 || status=1
  fi
  if [ "$ran" = true ] && [ "$status" -eq 0 ]; then
    quality_status=passed
  elif [ "$ran" = true ]; then
    quality_status=failed
    quality_excerpt="$(head -n "$GUARD_LINT_LINES" "$out" | cut -c1-"$GUARD_LINT_LINE_CHARS")"
  fi
  rm -f "$out"
}

guard_quality_marker() {
  local repository="${1:-}" excerpt="" tag=""
  if [ -n "$repository" ]; then
    tag=",\"repository\":\"$repository\""
  fi
  if [ -n "$quality_excerpt" ]; then
    excerpt=",\"excerpt\":\"$(printf '%s\n' "$quality_excerpt" | guard_json)\""
  fi
  printf '{"type":"matos","event":"quality","lint":"%s"%s%s}\n' "$quality_status" "$tag" "$excerpt"
}
