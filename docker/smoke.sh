#!/usr/bin/env bash
# Run a built image the way compose.yaml does and check what a user relies on:
# health, dashboard, client API, a settings change saved to the volume, the
# non-root user, a clean stop on SIGTERM, and the state surviving a restart.
set -euo pipefail
image=${1:?usage: bash docker/smoke.sh IMAGE}
name="douane-smoke-${RANDOM}-$$"
volume="${name}-data"
scratch=$(mktemp -d)
cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
  rm -rf "$scratch"
}
trap cleanup EXIT

user=$(docker image inspect -f '{{.Config.User}}' "$image")
[[ -n "$user" && "$user" != 0 && "$user" != root ]] || { echo "image runs as root ($user)"; exit 1; }

docker volume create "$volume" >/dev/null
docker run -d --name "$name" --cpus=1 --memory=256m -p 127.0.0.1::8787 \
  --mount "type=volume,source=$volume,target=/data" "$image" >/dev/null

ready() {
  for ((attempt = 0; attempt < 30; attempt++)); do
    if docker exec "$name" /usr/local/bin/douanectl health >/dev/null 2>&1; then
      base="http://$(docker port "$name" 8787/tcp | head -n1)"
      return
    fi
    sleep 1
  done
  docker logs "$name"
  return 1
}

ready
curl -fsS "$base/health" | grep -q '"service":"douane"'
curl -fsS "$base/admin" | grep -qi '<html'
curl -fsS "$base/v1/models" | grep -q '"data"'
curl -fsS -X PATCH "$base/admin/api/settings" -H 'Content-Type: application/json' \
  -d '{"quota_refresh_seconds":901}' | grep -q '"quota_refresh_seconds":901'

docker stop --time 20 "$name" >/dev/null
test "$(docker inspect -f '{{.State.ExitCode}}' "$name")" = 0
docker cp "$name:/data/douane.sqlite" "$scratch/" >/dev/null
docker cp "$name:/data/douane.toml" "$scratch/" >/dev/null
grep -q 'quota_refresh_seconds = 901' "$scratch/douane.toml"

docker start "$name" >/dev/null
ready
curl -fsS "$base/admin/api/settings" | grep -q '"quota_refresh_seconds":901'
echo 'Docker smoke passed: health, dashboard, client API, settings saved to /data, non-root user, clean stop, restart'
