#!/usr/bin/env bash
# Runs a release package once, the way a user would, before anyone downloads it:
# unpack, check the binaries are for the right architecture, start `./open-prism`, check that
# the web app answers and reaches the API, read the token, then stop it and check nothing is left.
#
# Usage: scripts/smoke-release.sh <open-prism-<version>-linux-<arch>.tar.gz>
# Uses ports 18787 (API) and 13737 (web) unless SMOKE_API_PORT / SMOKE_WEB_PORT say otherwise.
set -euo pipefail

die() {
  echo "smoke-release: $*" >&2
  exit 1
}

[ $# -eq 1 ] || die "usage: $0 <open-prism-<version>-linux-<arch>.tar.gz>"
archive=$1
[ -f "$archive" ] || die "$archive not found"
name=$(basename -- "$archive" .tar.gz)
[[ $name =~ ^open-prism-(.+)-linux-(x64|arm64)$ ]] || die "not a release package name: $name"
version=${BASH_REMATCH[1]}
arch=${BASH_REMATCH[2]}
api_port=${SMOKE_API_PORT:-18787}
web_port=${SMOKE_WEB_PORT:-13737}
web=http://127.0.0.1:$web_port

listening() { (exec 3<> "/dev/tcp/127.0.0.1/$1") 2> /dev/null; }
for port in "$api_port" "$web_port"; do
  if listening "$port"; then die "port $port is already in use"; fi
done

tmp=$(mktemp -d)
pid=
# The processes running this package's Node, found by their executable.
package_nodes() {
  local proc
  for proc in /proc/[0-9]*; do
    [ "$(readlink "$proc/exe" 2> /dev/null)" = "$dir/node/bin/node" ] && echo "${proc#/proc/}"
  done
  return 0
}
cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2> /dev/null; then kill -KILL "$pid" 2> /dev/null || true; fi
  if [ -n "${dir:-}" ]; then
    for node in $(package_nodes); do kill -KILL "$node" 2> /dev/null || true; done
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT
fail() {
  echo "smoke-release: $*" >&2
  if [ -f "$tmp/log" ]; then
    echo "--- output of ./open-prism:" >&2
    cat "$tmp/log" >&2
  fi
  exit 1
}

echo "Unpacking $archive"
tar -xzf "$archive" -C "$tmp"
dir=$tmp/$name
[ -d "$dir" ] || die "the package has no $name/ folder"

# 1. Binaries for the architecture in the name.
case "$arch" in x64) machine=x86-64 ;; arm64) machine=aarch64 ;; esac
for binary in bin/open-prism-api node/bin/node; do
  [ -x "$dir/$binary" ] || fail "$binary is missing or not executable"
  description=$(file -b "$dir/$binary")
  [[ $description == *"$machine"* ]] || fail "$binary is not an $arch executable: $description"
done

# 2. Start it like a user, without a browser or TeX.
echo "Starting ./open-prism on ports $api_port (API) and $web_port (web)"
started=$SECONDS
(
  cd "$tmp"
  exec env -u OPENPRISM_ACCESS_TOKEN -u OPENPRISM_OPEN OPENPRISM_NO_BROWSER=1 OPENPRISM_TEX_IMAGE=none \
    "$dir/open-prism" --port "$api_port" --web-port "$web_port" --data-dir "$tmp/data"
) > "$tmp/log" 2>&1 &
pid=$!
until grep -q "Open Prism is running" "$tmp/log"; do
  kill -0 "$pid" 2> /dev/null || fail "./open-prism stopped before it was ready"
  [ $((SECONDS - started)) -lt 60 ] || fail "not ready after 60 seconds"
  sleep 0.5
done
echo "Ready after $((SECONDS - started)) s"

# 3. The web app answers, and reaches the API.
status=$(curl -s -o /dev/null -w '%{http_code}' "$web/login") || true
[ "$status" = 200 ] || fail "GET /login answered $status, not 200"
health=$(curl -fsS "$web/api/health") || fail "GET /api/health through the web app failed"
[[ $health == *"\"version\":\"$version\""* ]] || fail "GET /api/health answered $health, not version $version"

# 4. The token, and the sign-in link that was printed with it.
token=$("$dir/open-prism" --data-dir "$tmp/data" print-token) || fail "print-token failed"
[[ $token =~ ^[0-9a-f]{64}$ ]] || fail "print-token printed \"$token\", not a token"
grep -qF "$web/login?token=$token" "$tmp/log" || fail "no sign-in link with the token in the output"

# 5. A clean stop.
kill -TERM "$pid"
stopping=$SECONDS
while kill -0 "$pid" 2> /dev/null; do
  [ $((SECONDS - stopping)) -lt 20 ] || fail "still running 20 seconds after SIGTERM"
  sleep 0.2
done
code=0
wait "$pid" || code=$?
pid=
[ "$code" = 0 ] || fail "./open-prism exited with $code after SIGTERM"
for port in "$api_port" "$web_port"; do
  if listening "$port"; then fail "port $port is still in use after the stop"; fi
done
left=$(package_nodes)
[ -z "$left" ] || fail "the package's Node is still running (process $left)"

echo "OK: $name starts, serves the web app and the API, and stops cleanly"
