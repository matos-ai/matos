#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d /tmp/guard-test.XXXXXX)"
trap 'rm -rf "$work"' EXIT

source "$here/guard.sh"

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

fresh_repo() {
  rm -rf "$work/repo"
  mkdir "$work/repo"
  cd "$work/repo"
  git init -q
  git config user.email t@example.test
  git config user.name t
  echo base >README.md
  git add -A
  git commit -q -m base
  GUARD_DENY_PATHS=$'.github/workflows/\ndeploy/\ncharts/\nhelm/\nterraform/\nk8s/\n.env*\n*.pem\n*.key\nid_rsa*\n*.p12\ncredentials*\n.matos.toml'
  GUARD_ALLOW_PATHS=""
  GUARD_MAX_FILES=50
  GUARD_MAX_LINES=2000
  PATH="$ORIGINAL_PATH"
}

ORIGINAL_PATH="$PATH"

outcome=""
outcome_of() {
  git add -A
  if guard_check; then outcome=passed; else outcome=blocked; fi
}

reasons() {
  printf '%s\n' ${guard_reasons[@]+"${guard_reasons[@]}"}
}

fresh_repo
mkdir -p .github/workflows
echo "name: ci" >.github/workflows/ci.yml
outcome_of
check "a workflow file is blocked" blocked "$outcome"
check "the path is named" "chemin sensible : .github/workflows/ci.yml" "$(reasons)"

GUARD_ALLOW_PATHS=".github/workflows/"
outcome_of
check "the same diff passes with allow_paths" passed "$outcome"

fresh_repo
mkdir -p src/deep/er
echo x >src/deep/er/server.pem
echo x >src/.env.local
mkdir notes
echo x >notes/id_rsa.pub
echo x >.matos.toml
outcome_of
check "file-name globs match at any depth" blocked "$outcome"
check "every sensitive path is listed" 4 "$(reasons | grep -c '^chemin sensible')"

fresh_repo
mkdir -p src/deploy docs
echo x >src/deploy/notes.md
echo x >docs/keys.md
outcome_of
check "a directory rule only matches at the top level" passed "$outcome"

fresh_repo
mkdir src
for i in $(seq 1 60); do echo "$i" >"src/f$i.txt"; done
outcome_of
check "60 files are blocked" blocked "$outcome"
check "the count is reported" "trop de fichiers : 60 (maximum 50)" "$(reasons)"
GUARD_MAX_FILES=80
outcome_of
check "a raised limit lets 60 files pass" passed "$outcome"

fresh_repo
seq 1 2500 >big.txt
outcome_of
check "2500 changed lines are blocked" blocked "$outcome"
GUARD_MAX_LINES=3000
outcome_of
check "a raised line limit lets them pass" passed "$outcome"

fresh_repo
head -c 2000 /dev/zero >blob.bin
outcome_of
check "a binary file counts as a file, not as lines" passed "$outcome"

fresh_repo
{ printf -- '-----BEGIN RSA PRIV'; printf 'ATE KEY-----\nabc\n'; } >config.txt
outcome_of
check "a private key is blocked" blocked "$outcome"
check "its kind and file are reported" "secret possible (cle privee) dans config.txt" "$(reasons)"

fresh_repo
printf 'token = "%s%s"\n' "ghp_" "abcdefghijklmnopqrstuvwxyz0123456789" >a.txt
printf 'key = %s%s\n' "AKIA" "ABCDEFGHIJKLMNOP" >b.txt
printf 'k = "%s%s"\n' "sk-ant-" "abcdefghijklmnopqrstuvwxyz" >c.txt
printf 'k = "%s%s"\n' "sk-proj-" "abcdefghijklmnopqrstuvwxyz" >d.txt
printf 'k = "%s%s"\n' "AIza" "abcdefghijklmnopqrstuvwxyz0123456789" >e.txt
printf 'k = "%s%s"\n' "xoxb-" "1234567890-abcdef" >f.txt
printf 'password = "hunter2hunter2"\n' >g.txt
outcome_of
check "every known kind is blocked" blocked "$outcome"
check "seven files are reported" 7 "$(reasons | grep -c '^secret possible')"
check "no secret value is ever reported" 0 "$(reasons | grep -c -e hunter2 -e ghp_ -e AKIA -e sk-)"

fresh_repo
printf 'password = ""\nlet secret = compute()\nsecret_name: "x"\n' >h.txt
outcome_of
check "empty or computed values are not secrets" passed "$outcome"

fresh_repo
printf 'AKIA%s\n' "ABCDEFGHIJKLMNOP" >old.txt
git add -A
git commit -q -m old
echo keep >old.txt
outcome_of
check "a secret that is only removed or untouched is not flagged" passed "$outcome"

fresh_repo
mkdir src
printf 'fn main() {}\n' >src/main.rs
printf '# readme\n' >>README.md
outcome_of
check "a clean diff passes" passed "$outcome"
check "a clean diff has no reason" 0 "$(reasons | grep -c . || true)"

fresh_repo
mkdir -p .github/workflows
echo 'name: "ci"' >".github/workflows/we\"ird.yml"
git add -A
guard_check || true
check "reasons are escaped into valid JSON" \
  '{"type":"matos","event":"guard_blocked","reasons":["chemin sensible : .github/workflows/we\"ird.yml"]}' \
  "$(guard_blocked_marker)"

fresh_repo
quality_status=x
guard_lint
check "without a toolchain the lint is not run" "not_run" "$quality_status"
check "its marker has no excerpt" '{"type":"matos","event":"quality","lint":"not_run"}' "$(guard_quality_marker)"

fresh_repo
stubs="$work/stubs"
mkdir -p "$stubs"
cat >"$stubs/cargo" <<'STUB'
#!/usr/bin/env bash
case "$1" in
  fmt) echo "// formatted" >>src.rs ;;
  clippy) echo 'warning: unused "x"'; echo "second line"; exit "${STUB_CLIPPY_STATUS:-0}" ;;
esac
STUB
chmod +x "$stubs/cargo"
printf '[package]\n' >Cargo.toml
echo "fn main() {}" >src.rs
git add -A
PATH="$stubs:$PATH"
guard_format
check "the formatter result is staged" "// formatted" "$(git show :src.rs | tail -n 1)"
guard_lint
check "a passing linter is reported as passed" passed "$quality_status"
STUB_CLIPPY_STATUS=1 guard_lint
check "a failing linter is reported as failed" failed "$quality_status"
check "its first lines travel in the marker" \
  '{"type":"matos","event":"quality","lint":"failed","excerpt":"warning: unused \"x\"\nsecond line"}' \
  "$(guard_quality_marker)"

if [ "$failures" -gt 0 ]; then
  echo "$failures failure(s)"
  exit 1
fi
echo "all guard checks passed"
