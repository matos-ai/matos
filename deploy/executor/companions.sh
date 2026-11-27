COMPANION_WORKSPACE="${COMPANION_WORKSPACE:-/workspace}"
COMPANION_TAB=$'\t'

companion_valid_repository() {
  [[ "$1" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || return 1
  case "/$1/" in */./* | */../*) return 1 ;; esac
}

companion_valid_path() {
  local path="$1" checkout="$COMPANION_WORKSPACE/repo"
  [[ "$path" =~ ^[A-Za-z0-9._/-]+$ ]] || return 1
  case "$path" in
    "$COMPANION_WORKSPACE"/*) ;;
    *) return 1 ;;
  esac
  case "$path/" in */./* | */../* | *//*) return 1 ;; esac
  case "$path/" in
    "$checkout"/*) return 1 ;;
  esac
}

companion_valid_ref() {
  [ -z "$1" ] && return 0
  [[ "$1" =~ ^[A-Za-z0-9._/-]+$ ]] || return 1
  case "$1" in -* | *..*) return 1 ;; esac
}

companion_clone_one() {
  local base="$1" repository="$2" path="$3" ref="$4" url
  url="$base/$repository.git"
  if [ -e "$path" ] && [ -n "$(ls -A "$path" 2>/dev/null)" ]; then
    return 1
  fi
  mkdir -p "$path" || return 1
  if [ -n "$ref" ]; then
    git -C "$path" init -q >&2 &&
      git -C "$path" remote add origin "$url" &&
      git -C "$path" fetch -q --depth 1 origin "$ref" >&2 &&
      git -C "$path" checkout -q --detach FETCH_HEAD >&2
  else
    git clone -q --depth 1 "$url" "$path" >&2
  fi || {
    rm -rf "$path"
    return 1
  }
  git -C "$path" remote set-url --push origin "no-push://companion-is-read-only" || {
    rm -rf "$path"
    return 1
  }
}

companion_names() {
  local out="" name
  for name in "$@"; do
    out="${out:+$out,}\"$name\""
  done
  printf '[%s]' "$out"
}

companions_clone() {
  local repository path ref cloned=() failed=()
  [ -n "${COMPANIONS:-}" ] || return 0
  : "${GIT_REMOTE_BASE:?}"
  while IFS="$COMPANION_TAB" read -r repository path ref || [ -n "$repository" ]; do
    [ -n "$repository" ] || continue
    if companion_valid_repository "$repository" && companion_valid_path "$path" &&
      companion_valid_ref "$ref" && companion_clone_one "$GIT_REMOTE_BASE" "$repository" "$path" "$ref"; then
      cloned+=("$repository")
    else
      echo "companion $repository was not cloned" >&2
      failed+=("${repository//[^A-Za-z0-9._\/-]/?}")
    fi
  done <<<"$COMPANIONS"
  printf '{"type":"matos","event":"companions","cloned":%s,"failed":%s}\n' \
    "$(companion_names ${cloned[@]+"${cloned[@]}"})" "$(companion_names ${failed[@]+"${failed[@]}"})"
}

write_registry_config() {
  if [ -n "${CARGO_REGISTRY_INDEX:-}" ]; then
    mkdir -p "$CARGO_HOME"
    cat >"$CARGO_HOME/config.toml" <<CARGO_CONFIG
[source.crates-io]
replace-with = "matos"

[source.matos]
registry = "sparse+$CARGO_REGISTRY_INDEX"
CARGO_CONFIG
  fi
  if [ -n "${NPM_REGISTRY:-}" ]; then
    printf 'registry=%s\n' "$NPM_REGISTRY" >"$HOME/.npmrc"
  fi
}
