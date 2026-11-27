#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d /tmp/repositories-test.XXXXXX)"
trap 'rm -rf "$work"' EXIT

export COMPANION_WORKSPACE="$work/workspace"
export HOME="$work/home"
export GIT_CONFIG_GLOBAL="$work/gitconfig"
export GIT_TERMINAL_PROMPT=0
mkdir -p "$HOME"
git config --global user.email t@example.test
git config --global user.name t
git config --global init.defaultBranch main
git config --global protocol.file.allow always

source "$here/guard.sh"
source "$here/companions.sh"
source "$here/repositories.sh"

failures=0
check() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "ok   $name"
  else
    echo "FAIL $name: expected [$expected], got [$actual]"
    failures=$((failures + 1))
  fi
}

tab=$'\t'
remote="$work/remote"
export GIT_REMOTE_BASE="file://$remote"
export GIT_REMOTE="file://$remote/acme/app.git"
export BRANCH="matos/run"
export COMMIT_FALLBACK_MESSAGE="fallback message"
export CONTRIBUTION_PATH="$COMPANION_WORKSPACE/.matos/contribution.md"
GUARD_DENY_PATHS=$'deploy/\n.env*'
GUARD_ALLOW_PATHS=""
GUARD_MAX_FILES=50
GUARD_MAX_LINES=2000

setup() {
  cd "$work"
  rm -rf "$remote" "$COMPANION_WORKSPACE" "$work/seed"
  mkdir -p "$remote/acme" "$COMPANION_WORKSPACE"
  local name
  for name in app sdk lib; do
    git init -q --bare "$remote/acme/$name.git"
    git init -q "$work/seed"
    echo "$name" >"$work/seed/$name.txt"
    git -C "$work/seed" add -A
    git -C "$work/seed" commit -q -m "$name"
    git -C "$work/seed" push -q "$remote/acme/$name.git" HEAD:refs/heads/main
    rm -rf "$work/seed"
  done
  git clone -q --depth 50 --branch main "$GIT_REMOTE" "$COMPANION_WORKSPACE/repo"
  mkdir -p "$COMPANION_WORKSPACE/.matos/contribution"
  EXTRA_REPOSITORIES="acme/sdk${tab}$COMPANION_WORKSPACE/sdk${tab}main${tab}10${tab}100${tab}"$'\n'"acme/lib${tab}$COMPANION_WORKSPACE/lib${tab}main${tab}10${tab}100${tab}deploy/,docs/"
  export EXTRA_REPOSITORIES
  cd "$COMPANION_WORKSPACE/repo"
  base_sha="$(git rev-parse HEAD)"
  extras_clone >"$work/extras_marker" 2>/dev/null
  extras_marker="$(cat "$work/extras_marker")"
}

pushed() {
  git --git-dir="$remote/acme/$1.git" rev-parse --verify -q "refs/heads/$BRANCH" >/dev/null && echo yes || echo no
}

publish() {
  rc=0
  out="$(publish_changes "$base_sha" 2>"$work/stderr")" || rc=$?
}

count() {
  printf '%s\n' "$out" | grep -c "$1" || true
}

check "valid line shapes: repository" 0 "$(extra_valid_repository acme/sdk && echo 0 || echo 1)"
for bad in "acme" "acme/.." "a b/c" "acme/s;dk"; do
  check "repository [$bad] is refused" 1 "$(extra_valid_repository "$bad" && echo 0 || echo 1)"
done
for bad in "/etc/x" "$COMPANION_WORKSPACE/repo" "$COMPANION_WORKSPACE/repo/x" "$COMPANION_WORKSPACE/../x" "sdk"; do
  check "path [$bad] is refused" 1 "$(extra_valid_path "$bad" && echo 0 || echo 1)"
done
for bad in "" "-x" "a..b" "a b"; do
  check "branch [$bad] is refused" 1 "$(extra_valid_branch "$bad" && echo 0 || echo 1)"
done
check "a non numeric limit is refused" 1 "$(extra_valid_number ten && echo 0 || echo 1)"
check "allow paths with a space are refused" 1 "$(extra_valid_allow 'a b' && echo 0 || echo 1)"

setup
check "the primary label comes from its remote" acme/app "$(primary_label)"
check "the extra clone marker lists both" \
  '{"type":"matos","event":"extra_repositories","cloned":["acme/sdk","acme/lib"],"failed":[]}' "$extras_marker"
check "an extra is on the run branch" "$BRANCH" "$(git -C "$COMPANION_WORKSPACE/sdk" branch --show-current)"
check "an extra has a pushable remote" "file://$remote/acme/sdk.git" "$(git -C "$COMPANION_WORKSPACE/sdk" remote get-url --push origin)"

bad_line="acme/sdk${tab}$COMPANION_WORKSPACE/x${tab}main${tab}ten${tab}100${tab}"
rm -rf "$COMPANION_WORKSPACE/sdk" "$COMPANION_WORKSPACE/lib"
rc=0
bad_out="$(EXTRA_REPOSITORIES="$bad_line" extras_clone 2>"$work/stderr")" || rc=$?
check "an invalid line fails the run" 1 "$rc"
check "an invalid line is reported" \
  '{"type":"matos","event":"extra_repositories","cloned":[],"failed":["acme/sdk"]}' "$bad_out"
check "the error is on stderr" 1 "$(grep -c 'required' "$work/stderr")"
rc=0
bad_out="$(EXTRA_REPOSITORIES="acme/missing${tab}$COMPANION_WORKSPACE/m${tab}main${tab}1${tab}1${tab}" extras_clone 2>/dev/null)" || rc=$?
check "a clone that fails fails the run" 1 "$rc"
check "and leaves nothing behind" no "$([ -e "$COMPANION_WORKSPACE/m" ] && echo yes || echo no)"
rc=0
(unset EXTRA_REPOSITORIES; extras_clone >/dev/null) || rc=$?
check "no extra, no marker, success" 0 "$rc"

setup
echo changed >"$COMPANION_WORKSPACE/repo/app.txt"
echo changed >"$COMPANION_WORKSPACE/sdk/sdk.txt"
printf '# Shared title\n\nshared body\n' >"$CONTRIBUTION_PATH"
printf '# Sdk title\n\nsdk body\n' >"$COMPANION_WORKSPACE/.matos/contribution/acme__sdk.md"
publish
check "two changed repositories succeed" 0 "$rc"
check "the primary is pushed" yes "$(pushed app)"
check "the changed extra is pushed" yes "$(pushed sdk)"
check "the unchanged extra is not pushed" no "$(pushed lib)"
check "one pushed marker for the primary" 1 "$(count '"event":"pushed","sha":"[0-9a-f]*"}')"
check "one pushed marker for the extra with its repository" 1 "$(count '"event":"pushed","sha":"[0-9a-f]*","repository":"acme/sdk"}')"
check "no pushed marker for the unchanged one" 0 "$(count 'acme/lib')"
check "no no_changes marker" 0 "$(count no_changes)"
check "the extra quality marker names the repository" 1 "$(count '"event":"quality","lint":"not_run","repository":"acme/sdk"')"
check "the primary quality marker has no repository" 1 "$(count '"event":"quality","lint":"not_run"}')"
check "the primary commit uses the shared contribution" "Shared title" "$(git --git-dir="$remote/acme/app.git" log -1 --format=%s "$BRANCH")"
check "the extra commit uses its own contribution" "Sdk title" "$(git --git-dir="$remote/acme/sdk.git" log -1 --format=%s "$BRANCH")"
check "the shared contribution is emitted once without repository" 1 "$(count '"event":"contribution_end","sha256"')"
check "the per-repository contribution carries its repository" 1 "$(count '"event":"contribution_end","repository":"acme/sdk"')"
check "its chunk carries it too" 1 "$(count '"event":"contribution_chunk","repository":"acme/sdk"')"
check "the extra commit is one commit on its base" 1 "$(git -C "$COMPANION_WORKSPACE/sdk" rev-list --count "$(git -C "$COMPANION_WORKSPACE/sdk" rev-parse HEAD~1)..HEAD")"

setup
echo changed >"$COMPANION_WORKSPACE/repo/app.txt"
printf '# Shared title\n\nshared body\n' >"$CONTRIBUTION_PATH"
publish
check "only the primary changed, it is pushed" yes "$(pushed app)"
check "the extras are not" nono "$(pushed sdk)$(pushed lib)"
check "no repository on its pushed marker" 0 "$(count '"repository":"acme/app"')"

setup
echo changed >"$COMPANION_WORKSPACE/repo/app.txt"
mkdir -p "$COMPANION_WORKSPACE/sdk/deploy"
echo x >"$COMPANION_WORKSPACE/sdk/deploy/job.yaml"
publish
check "a block on an extra exits 3" 3 "$rc"
check "nothing is pushed for the primary" no "$(pushed app)"
check "nor for the extra" no "$(pushed sdk)"
check "one guard_blocked marker" 1 "$(count guard_blocked)"
check "its reason names the repository" 1 "$(count 'acme/sdk: chemin sensible : deploy/job.yaml')"
check "no pushed marker" 0 "$(count pushed)"

setup
echo changed >"$COMPANION_WORKSPACE/repo/app.txt"
mkdir -p "$COMPANION_WORKSPACE/lib/docs"
echo x >"$COMPANION_WORKSPACE/lib/docs/a.md"
mkdir -p "$COMPANION_WORKSPACE/lib/deploy"
echo x >"$COMPANION_WORKSPACE/lib/deploy/job.yaml"
publish
check "the extra's own allow paths are used" 0 "$rc"
check "the extra with an allowed path is pushed" yes "$(pushed lib)"

setup
echo changed >"$COMPANION_WORKSPACE/repo/app.txt"
seq 1 200 >"$COMPANION_WORKSPACE/sdk/big.txt"
publish
check "the extra's own line limit is used" 3 "$rc"
check "the reason is prefixed" 1 "$(count 'acme/sdk: ')"
check "the primary is not pushed" no "$(pushed app)"

setup
mkdir -p "$COMPANION_WORKSPACE/repo/deploy"
echo x >"$COMPANION_WORKSPACE/repo/deploy/a.yaml"
publish
check "a block on the primary is prefixed with its repository" 1 "$(count 'acme/app: chemin sensible')"

setup
publish
check "no change anywhere succeeds" 0 "$rc"
check "a single no_changes marker" '{"type":"matos","event":"no_changes"}' "$out"
check "nothing is pushed" nonono "$(pushed app)$(pushed sdk)$(pushed lib)"

setup
unset EXTRA_REPOSITORIES
extra_repos=()
extra_paths=()
extra_files=()
extra_lines=()
extra_allow=()
extra_bases=()
echo changed >"$COMPANION_WORKSPACE/repo/app.txt"
publish
check "without extras the primary is pushed as before" yes "$(pushed app)"
check "the pushed marker is the historic shape" 1 "$(count '^{"type":"matos","event":"pushed","sha":"[0-9a-f]*"}$')"
setup
unset EXTRA_REPOSITORIES
extra_repos=()
extra_paths=()
extra_files=()
extra_lines=()
extra_allow=()
extra_bases=()
mkdir -p "$COMPANION_WORKSPACE/repo/deploy"
echo x >"$COMPANION_WORKSPACE/repo/deploy/a.yaml"
publish
check "without extras a block reason is not prefixed" 1 "$(count '"reasons":\["chemin sensible : deploy/a.yaml"\]')"

if [ "$failures" -gt 0 ]; then
  echo "$failures failure(s)"
  exit 1
fi
echo "all repository checks passed"
