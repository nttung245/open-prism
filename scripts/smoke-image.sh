#!/usr/bin/env bash
# Runs a Docker image of Open Prism once, the way a user would, before it is published: start it
# with a fresh volume, sign in, compile the article template to a PDF with the TeX Live inside,
# check the published port and the health check, stop it, then start a new container on the same
# volume and check the token and the project are still there.
#
# Usage: scripts/smoke-image.sh <image>
# Publishes the web app on 127.0.0.1:13737 unless SMOKE_WEB_PORT says otherwise.
set -euo pipefail

die() {
  echo "smoke-image: $*" >&2
  exit 1
}

[ $# -eq 1 ] || die "usage: $0 <image>"
image=$1
port=${SMOKE_WEB_PORT:-13737}
web=http://127.0.0.1:$port
docker image inspect "$image" > /dev/null 2>&1 || die "no image $image: build it first"
if (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null; then die "port $port is already in use"; fi

name=open-prism-smoke-$$
container=$name
volume=$name
tmp=$(mktemp -d)
cleanup() {
  docker rm -f "$container" > /dev/null 2>&1 || true
  docker volume rm -f "$volume" > /dev/null 2>&1 || true
  rm -rf "$tmp"
}
trap cleanup EXIT
fail() {
  echo "smoke-image: $*" >&2
  echo "--- log of the container:" >&2
  docker logs "$container" >&2 2>&1 || true
  exit 1
}
# A value from the JSON on stdin: `json 'data["id"]'`.
json() { python3 -c "import json, sys; data = json.load(sys.stdin); print($1)"; }

start() {
  docker run -d --name "$container" --init -p "127.0.0.1:$port:3737" -v "$volume:/data" \
    --health-interval 2s --health-start-period 60s "$image" > /dev/null
  local since=$SECONDS
  until docker logs "$container" 2>&1 | grep -q "Open Prism is running"; do
    [ "$(docker inspect -f '{{.State.Running}}' "$container")" = true ] ||
      fail "the container stopped before it was ready"
    [ $((SECONDS - since)) -lt 120 ] || fail "not ready after 120 seconds"
    sleep 1
  done
  echo "Ready after $((SECONDS - since)) s"
}

stop() {
  local since=$SECONDS code
  docker stop -t 10 "$container" > /dev/null
  code=$(docker inspect -f '{{.State.ExitCode}}' "$container")
  # 137 means Docker gave up waiting and killed it.
  [ "$code" = 0 ] || fail "the container exited with $code after docker stop"
  echo "Stopped in $((SECONDS - since)) s"
  docker rm "$container" > /dev/null
}

read_token() {
  docker exec "$container" open-prism print-token | grep -Eo '^[0-9a-f]{64}$' | tail -1
}

api() { curl -fsS -H "Authorization: Bearer $token" "$@"; }

echo "Starting $image on 127.0.0.1:$port with a new volume"
docker volume create "$volume" > /dev/null
start

# Only the web app, only on the host's loopback; not as root.
published=$(docker port "$container")
[ "$published" = "3737/tcp -> 127.0.0.1:$port" ] || fail "unexpected published ports: $published"
uid=$(docker exec "$container" id -u)
[ "$uid" != 0 ] || fail "the app runs as root"

status=$(curl -s -o /dev/null -w '%{http_code}' "$web/login") || true
[ "$status" = 200 ] || fail "GET /login answered $status, not 200"
engines=$(curl -fsS "$web/api/health" | json '" ".join(e["kind"] for e in data["engines"])') ||
  fail "GET /api/health failed"
[[ " $engines " == *" latexmk "* ]] || fail "no latexmk among the TeX engines ($engines)"

token=$(read_token)
[ -n "$token" ] || fail "print-token printed no token"
docker logs "$container" 2>&1 | grep -qF "http://127.0.0.1:3737/login?token=$token" ||
  fail "no sign-in link with the token in the log"

# Compile the article template with the TeX Live in the image.
project=$(api -X POST -H 'content-type: application/json' \
  -d '{"name":"Smoke test","template":"article"}' "$web/api/projects" | json 'data["id"]') ||
  fail "cannot create a project"
run=$(api -X POST "$web/api/projects/$project/compile" | json 'data["runId"]') ||
  fail "cannot start a compilation"
since=$SECONDS
until result=$(api "$web/api/projects/$project/compile/$run" 2> /dev/null); do
  [ $((SECONDS - since)) -lt 180 ] || fail "the compilation did not finish in 180 seconds"
  sleep 1
done
outcome=$(json 'data["status"], data["hasPdf"], data["message"]' <<< "$result")
[ "$outcome" = "success True None" ] || fail "the compilation ended with: $outcome"
api -o "$tmp/output.pdf" "$web/api/projects/$project/runs/$run/output.pdf" || fail "cannot download the PDF"
[ "$(head -c 4 "$tmp/output.pdf")" = "%PDF" ] || fail "the downloaded file is not a PDF"
echo "Compiled the article template in $((SECONDS - since)) s ($(wc -c < "$tmp/output.pdf") bytes of PDF)"

since=$SECONDS
until [ "$(docker inspect -f '{{.State.Health.Status}}' "$container")" = healthy ]; do
  [ $((SECONDS - since)) -lt 60 ] || fail "the health check did not report healthy"
  sleep 1
done

stop

# A new container on the same volume: same token, same project.
start
[ "$(read_token)" = "$token" ] || fail "the access token changed with a new container"
api "$web/api/projects" | json '[p["id"] for p in data]' | grep -qF "$project" ||
  fail "the project is gone with a new container"
stop

echo "OK: $image compiles a PDF, keeps its data in the volume, and stops cleanly"
