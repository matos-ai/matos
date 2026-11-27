REPORT_MAX_BYTES=1048576
REPORT_CHUNK_CHARS=4096
REPORT_NOTE=$'\n\n_[rapport tronque a 1 Mio]_\n'

EXTRA_TAB=$'\t'
extra_repos=()
extra_paths=()
extra_files=()
extra_lines=()
extra_allow=()
extra_bases=()

marker() {
  printf '{"type":"matos","event":"%s"%s}\n' "$1" "${2:-}"
}

# Sends a document on stdout as base64 chunks, then its SHA-256 and size;
# one above REPORT_MAX_BYTES is cut and ends with a note. The second argument
# names it in the markers: report (report_chunk, report_end) or plan. The
# third, when given, is the owner/name the document belongs to.
emit_report() {
  local source="$1" doc="${2:-report}" repository="${3:-}" sent truncated=false size keep tag=""
  [ -s "$source" ] || return 1
  if [ -n "$repository" ]; then
    tag=",\"repository\":\"$repository\""
  fi
  sent="$(mktemp /tmp/report.XXXXXX)"
  size="$(wc -c <"$source" | tr -d ' ')"
  if [ "$size" -gt "$REPORT_MAX_BYTES" ]; then
    keep=$((REPORT_MAX_BYTES - $(printf '%s' "$REPORT_NOTE" | LC_ALL=C wc -c | tr -d ' ')))
    head -c "$keep" "$source" >"$sent"
    printf '%s' "$REPORT_NOTE" >>"$sent"
    truncated=true
  else
    cat "$source" >"$sent"
  fi
  base64 -w0 <"$sent" | fold -w "$REPORT_CHUNK_CHARS" | while IFS= read -r chunk || [ -n "$chunk" ]; do
    marker "${doc}_chunk" "$tag,\"data\":\"$chunk\""
  done
  marker "${doc}_end" "$tag,\"sha256\":\"$(sha256sum "$sent" | cut -d' ' -f1)\",\"bytes\":$(wc -c <"$sent" | tr -d ' '),\"truncated\":$truncated"
  rm -f "$sent"
}

# The title and body of the file in $1 as a commit message in $2, when the
# file has a title and holds none of the terms that may not leave the platform.
contribution_message() {
  local source="$1" out="$2" terms
  [ -s "$source" ] || return 1
  awk '
    !titled && /[^[:space:]]/ {
      sub(/^[[:space:]]*#*[[:space:]]*/, ""); sub(/[[:space:]]+$/, "")
      if ($0 == "") exit 1
      print; print ""; titled = 1; next
    }
    titled { print }
    END { if (!titled) exit 1 }
  ' "$source" >"$out" || return 1
  terms="$(mktemp /tmp/terms.XXXXXX)"
  printf '%s\n' "${LEAK_TERMS:-}" | grep -v '^[[:space:]]*$' >"$terms" || true
  if [ -s "$terms" ] && grep -qiF -f "$terms" "$out"; then
    echo "the contribution text names what may not leave the platform, the fallback message is used" >&2
    rm -f "$terms"
    return 1
  fi
  rm -f "$terms"
}

extra_valid_repository() {
  [[ "$1" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || return 1
  case "/$1/" in */./* | */../*) return 1 ;; esac
}

extra_valid_path() {
  local path="$1" workspace="${COMPANION_WORKSPACE:-/workspace}"
  [[ "$path" =~ ^[A-Za-z0-9._/-]+$ ]] || return 1
  case "$path" in
    "$workspace"/*) ;;
    *) return 1 ;;
  esac
  case "$path/" in */./* | */../* | *//*) return 1 ;; esac
  case "$path/" in
    "$workspace/repo"/*) return 1 ;;
  esac
}

extra_valid_branch() {
  [[ "$1" =~ ^[A-Za-z0-9._/-]+$ ]] || return 1
  case "$1" in -* | *..*) return 1 ;; esac
}

extra_valid_number() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

extra_valid_allow() {
  [[ "$1" =~ ^[A-Za-z0-9._/*,-]*$ ]]
}

extra_names() {
  local out="" name
  for name in "$@"; do
    out="${out:+$out,}\"$name\""
  done
  printf '[%s]' "$out"
}

extra_clone_one() {
  local repository="$1" path="$2" base="$3"
  if [ -e "$path" ] && [ -n "$(ls -A "$path" 2>/dev/null)" ]; then
    return 1
  fi
  git clone -q --depth 50 --branch "$base" "$GIT_REMOTE_BASE/$repository.git" "$path" >&2 &&
    git -C "$path" switch -q -c "$BRANCH" >&2 || {
    rm -rf "$path"
    return 1
  }
}

# Clones the repositories of EXTRA_REPOSITORIES, one per line:
# owner/name, path, base branch, max files, max lines, allowed paths
# (comma-separated, may be empty). They are required: a failure is reported
# in the marker and returns non-zero.
extras_clone() {
  local repository path base files lines allow cloned=() failed=()
  extra_repos=()
  extra_paths=()
  extra_files=()
  extra_lines=()
  extra_allow=()
  extra_bases=()
  [ -n "${EXTRA_REPOSITORIES:-}" ] || return 0
  : "${GIT_REMOTE_BASE:?}" "${BRANCH:?}"
  while IFS="$EXTRA_TAB" read -r repository path base files lines allow || [ -n "$repository" ]; do
    [ -n "$repository" ] || continue
    if extra_valid_repository "$repository" && extra_valid_path "$path" &&
      extra_valid_branch "$base" && extra_valid_number "$files" && extra_valid_number "$lines" &&
      extra_valid_allow "$allow" && extra_clone_one "$repository" "$path" "$base"; then
      cloned+=("$repository")
      extra_repos+=("$repository")
      extra_paths+=("$path")
      extra_files+=("$files")
      extra_lines+=("$lines")
      extra_allow+=("$allow")
      extra_bases+=("$(git -C "$path" rev-parse HEAD)")
    else
      echo "extra repository ${repository:-?} was not cloned" >&2
      failed+=("${repository//[^A-Za-z0-9._\/-]/?}")
    fi
  done <<<"$EXTRA_REPOSITORIES"
  printf '{"type":"matos","event":"extra_repositories","cloned":%s,"failed":%s}\n' \
    "$(extra_names ${cloned[@]+"${cloned[@]}"})" "$(extra_names ${failed[@]+"${failed[@]}"})"
  if [ "${#failed[@]}" -gt 0 ]; then
    echo "extra repositories are required and ${#failed[@]} could not be cloned: ${failed[*]}" >&2
    return 1
  fi
}

# owner/name of the primary repository, read from its remote.
primary_label() {
  local url="${GIT_REMOTE:-}" name owner
  url="${url%/}"
  url="${url%.git}"
  name="${url##*/}"
  url="${url%/*}"
  owner="${url##*/}"
  if [ -n "$owner" ] && [ -n "$name" ] && [ "$owner" != "$url" ]; then
    printf '%s/%s\n' "$owner" "$name"
  else
    printf 'repo\n'
  fi
}

publish_stage() {
  git reset -q --soft "$1"
  git add -A
  if git diff --cached --quiet; then
    return 1
  fi
  guard_format
  ! git diff --cached --quiet
}

publish_commit_message() {
  local label="$1" out="$2" file
  file="$(dirname "$CONTRIBUTION_PATH")/contribution/${label%%/*}__${label#*/}.md"
  contribution_message "$file" "$out" || contribution_message "$CONTRIBUTION_PATH" "$out" ||
    printf '%s\n' "$COMMIT_FALLBACK_MESSAGE" >"$out"
}

# Change mode, all or nothing, from the primary checkout (the current
# directory, based on $1) and every extra repository. Returns 3 with one
# guard_blocked marker and nothing pushed when any changed repository is
# blocked.
publish_changes() {
  local primary_base="$1" primary primary_dir total i changed=() any=false
  local labels=() dirs=() bases=() maxfiles=() maxlines=() allows=()
  local reasons=() j message shared_sent=false
  primary="$(primary_label)"
  primary_dir="$(pwd)"
  labels=("$primary" ${extra_repos[@]+"${extra_repos[@]}"})
  dirs=("$primary_dir" ${extra_paths[@]+"${extra_paths[@]}"})
  bases=("$primary_base" ${extra_bases[@]+"${extra_bases[@]}"})
  maxfiles=("$GUARD_MAX_FILES" ${extra_files[@]+"${extra_files[@]}"})
  maxlines=("$GUARD_MAX_LINES" ${extra_lines[@]+"${extra_lines[@]}"})
  allows=("${GUARD_ALLOW_PATHS:-}" ${extra_allow[@]+"${extra_allow[@]}"})
  total="${#labels[@]}"

  for ((i = 0; i < total; i++)); do
    cd "${dirs[$i]}"
    if publish_stage "${bases[$i]}"; then
      changed+=(1)
      any=true
    else
      changed+=(0)
    fi
  done
  cd "$primary_dir"
  if [ "$any" = false ]; then
    marker no_changes
    return 0
  fi

  for ((i = 0; i < total; i++)); do
    [ "${changed[$i]}" = 1 ] || continue
    cd "${dirs[$i]}"
    if [ "$i" -eq 0 ]; then
      guard_check || true
    else
      GUARD_MAX_FILES="${maxfiles[$i]}" GUARD_MAX_LINES="${maxlines[$i]}" \
        GUARD_ALLOW_PATHS="${allows[$i]//,/$'\n'}" guard_check || true
    fi
    for ((j = 0; j < ${#guard_reasons[@]}; j++)); do
      if [ "$total" -gt 1 ]; then
        reasons+=("${labels[$i]}: ${guard_reasons[$j]}")
      else
        reasons+=("${guard_reasons[$j]}")
      fi
    done
  done
  cd "$primary_dir"
  if [ "${#reasons[@]}" -gt 0 ]; then
    guard_reasons=("${reasons[@]}")
    guard_blocked_marker
    echo "the diff is not pushed: ${guard_reasons[*]}" >&2
    return 3
  fi

  for ((i = 0; i < total; i++)); do
    [ "${changed[$i]}" = 1 ] || continue
    cd "${dirs[$i]}"
    guard_lint
    if [ "$i" -eq 0 ]; then
      guard_quality_marker
    else
      guard_quality_marker "${labels[$i]}"
    fi
    message="$(mktemp /tmp/message.XXXXXX)"
    publish_commit_message "${labels[$i]}" "$message"
    git commit -q --cleanup=whitespace -F "$message" >&2
    rm -f "$message"
    git push origin "HEAD:refs/heads/$BRANCH" >&2
    if [ "$shared_sent" = false ]; then
      emit_report "$CONTRIBUTION_PATH" contribution || true
      shared_sent=true
    fi
    emit_report "$(dirname "$CONTRIBUTION_PATH")/contribution/${labels[$i]%%/*}__${labels[$i]#*/}.md" \
      contribution "${labels[$i]}" || true
    if [ "$i" -eq 0 ]; then
      marker pushed ",\"sha\":\"$(git rev-parse HEAD)\""
    else
      marker pushed ",\"sha\":\"$(git rev-parse HEAD)\",\"repository\":\"${labels[$i]}\""
    fi
  done
  cd "$primary_dir"
}
