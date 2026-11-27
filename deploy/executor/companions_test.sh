#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d /tmp/companions-test.XXXXXX)"
trap 'rm -rf "$work"' EXIT

export COMPANION_WORKSPACE="$work/workspace"
export HOME="$work/home"
export GIT_CONFIG_GLOBAL="$work/gitconfig"
export GIT_TERMINAL_PROMPT=0
mkdir -p "$COMPANION_WORKSPACE/repo" "$HOME"
git config --global user.email t@example.test
git config --global user.name t
git config --global init.defaultBranch main
git config --global protocol.file.allow always

source "$here/companions.sh"

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

remote="$work/remote"
mkdir -p "$remote/acme"
git init -q --bare "$remote/acme/sdk.git"
git init -q --bare "$remote/acme/docs.git"
seed="$work/seed"
git init -q "$seed"
echo one >"$seed/lib.txt"
git -C "$seed" add -A
git -C "$seed" commit -q -m one
first="$(git -C "$seed" rev-parse HEAD)"
echo two >"$seed/lib.txt"
git -C "$seed" commit -q -am two
git -C "$seed" push -q "$remote/acme/sdk.git" HEAD:refs/heads/main
git -C "$seed" push -q "$remote/acme/sdk.git" "$first:refs/heads/pinned"
git -C "$seed" push -q "$remote/acme/docs.git" HEAD:refs/heads/main
git -C "$remote/acme/sdk.git" config uploadpack.allowAnySHA1InWant true

export GIT_REMOTE_BASE="file://$remote"
tab=$'\t'

clone() {
  COMPANIONS="$1" companions_clone 2>/dev/null
}

check "a valid repository is accepted" 0 "$(companion_valid_repository acme/sdk && echo 0 || echo 1)"
for bad in "acme" "acme/" "/sdk" "acme/.." "acme/." "acme/../x" "acme/sdk/x" "ac me/sdk" "acme/s;dk" "../sdk"; do
  check "repository [$bad] is refused" 1 "$(companion_valid_repository "$bad" && echo 0 || echo 1)"
done
check "a sibling path is accepted" 0 "$(companion_valid_path "$COMPANION_WORKSPACE/sdk" && echo 0 || echo 1)"
for bad in "/etc" "$COMPANION_WORKSPACE" "$COMPANION_WORKSPACE/repo" "$COMPANION_WORKSPACE/repo/sdk" \
  "$COMPANION_WORKSPACE/../x" "$COMPANION_WORKSPACE/a/../b" "$COMPANION_WORKSPACE//x" "$COMPANION_WORKSPACE/a b" "sdk"; do
  check "path [$bad] is refused" 1 "$(companion_valid_path "$bad" && echo 0 || echo 1)"
done
for bad in "-x" "a..b" "a b" 'a;b'; do
  check "ref [$bad] is refused" 1 "$(companion_valid_ref "$bad" && echo 0 || echo 1)"
done
check "an empty ref is accepted" 0 "$(companion_valid_ref "" && echo 0 || echo 1)"

marker_line="$(clone "acme/sdk${tab}$COMPANION_WORKSPACE/sdk${tab}")"
check "the default branch is cloned" two "$(cat "$COMPANION_WORKSPACE/sdk/lib.txt")"
check "its marker lists it" '{"type":"matos","event":"companions","cloned":["acme/sdk"],"failed":[]}' "$marker_line"
check "the clone is shallow" 1 "$(git -C "$COMPANION_WORKSPACE/sdk" rev-list --count HEAD)"
check "its push url is unusable" "no-push://companion-is-read-only" "$(git -C "$COMPANION_WORKSPACE/sdk" remote get-url --push origin)"
check "its fetch url is the proxy route without a token" "file://$remote/acme/sdk.git" "$(git -C "$COMPANION_WORKSPACE/sdk" remote get-url origin)"
if git -C "$COMPANION_WORKSPACE/sdk" push origin HEAD:refs/heads/x >/dev/null 2>&1; then
  check "a push from the companion fails" refused pushed
else
  check "a push from the companion fails" refused refused
fi
check "no token is left in its git config" 0 "$(grep -ci 'token\|password\|x-access' "$COMPANION_WORKSPACE/sdk/.git/config" || true)"

rm -rf "$COMPANION_WORKSPACE/sdk"
marker_line="$(clone "acme/sdk${tab}$COMPANION_WORKSPACE/sdk${tab}pinned")"
check "a branch ref is fetched and checked out detached" one "$(cat "$COMPANION_WORKSPACE/sdk/lib.txt")"
check "the checkout is detached" "" "$(git -C "$COMPANION_WORKSPACE/sdk" branch --show-current)"
check "the ref case reports it too" '{"type":"matos","event":"companions","cloned":["acme/sdk"],"failed":[]}' "$marker_line"

rm -rf "$COMPANION_WORKSPACE/sdk"
clone "acme/sdk${tab}$COMPANION_WORKSPACE/sdk${tab}$first" >/dev/null
check "a commit ref is fetched at that commit" "$first" "$(git -C "$COMPANION_WORKSPACE/sdk" rev-parse HEAD)"

rm -rf "$COMPANION_WORKSPACE/sdk"
lines="acme/sdk${tab}$COMPANION_WORKSPACE/sdk${tab}main"$'\n'"acme/missing${tab}$COMPANION_WORKSPACE/missing${tab}"$'\n'"acme/docs${tab}$COMPANION_WORKSPACE/docs${tab}nope"$'\n'"acme/docs${tab}$COMPANION_WORKSPACE/repo/docs${tab}"
marker_line="$(clone "$lines")"
check "a failing clone is reported and the others still happen" \
  '{"type":"matos","event":"companions","cloned":["acme/sdk"],"failed":["acme/missing","acme/docs","acme/docs"]}' "$marker_line"
check "a failed clone leaves nothing behind" "no" "$([ -e "$COMPANION_WORKSPACE/missing" ] && echo yes || echo no)"
check "an unknown ref leaves nothing behind" "no" "$([ -e "$COMPANION_WORKSPACE/docs" ] && echo yes || echo no)"
check "nothing is written inside the checkout" "no" "$([ -e "$COMPANION_WORKSPACE/repo/docs" ] && echo yes || echo no)"

check "no companion, no marker" "" "$(COMPANIONS="" companions_clone)"

mkdir -p "$COMPANION_WORKSPACE/taken"
echo x >"$COMPANION_WORKSPACE/taken/file"
marker_line="$(clone "acme/docs${tab}$COMPANION_WORKSPACE/taken${tab}")"
check "a path that is not empty is not overwritten" x "$(cat "$COMPANION_WORKSPACE/taken/file")"
check "and is reported" '{"type":"matos","event":"companions","cloned":[],"failed":["acme/docs"]}' "$marker_line"

CARGO_HOME="$work/cargo" CARGO_REGISTRY_INDEX="http://proxy:4130/crates-index/" NPM_REGISTRY="http://proxy:4130/npm/" write_registry_config
check "cargo replaces crates.io by the proxy's sparse index" \
  $'[source.crates-io]\nreplace-with = "matos"\n\n[source.matos]\nregistry = "sparse+http://proxy:4130/crates-index/"' \
  "$(cat "$work/cargo/config.toml")"
check "npm uses the proxy's registry" "registry=http://proxy:4130/npm/" "$(cat "$HOME/.npmrc")"
rm -rf "$work/cargo" "$HOME/.npmrc"
CARGO_HOME="$work/cargo" CARGO_REGISTRY_INDEX="" NPM_REGISTRY="" write_registry_config
check "without a registry base nothing is written" "no" "$([ -e "$work/cargo" ] || [ -e "$HOME/.npmrc" ] && echo yes || echo no)"
inside=no
case "$work/cargo $HOME/.npmrc" in "$COMPANION_WORKSPACE/repo"*) inside=yes ;; esac
check "the registry files are outside the checkout" no "$inside"

if [ "$failures" -gt 0 ]; then
  echo "$failures failure(s)"
  exit 1
fi
echo "all companion checks passed"
