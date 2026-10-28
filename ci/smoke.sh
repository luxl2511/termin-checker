#!/usr/bin/env bash
set -euo pipefail

# NOTE: this overwrites the config and installs the real background service, so we only run it on throwaway CI runners.
if [ "${CI:-}" != true ]; then
  echo "ci/smoke.sh only runs in CI, it overwrites ~/.config/termin-checker/config.toml" >&2
  exit 1
fi

bin="$1"
cfg_dir="$HOME/.config/termin-checker"
mkdir -p "$cfg_dir"
deadline=$(date -d '+7 days' +%d.%m.%Y 2>/dev/null || date -v+7d +%d.%m.%Y)
cat > "$cfg_dir/config.toml" <<EOF
provider = "bochum"
service_name = "Kfz - Kurzzeitkennzeichen"
location_name = "Kfz-Zulassung Bochum"
calendar_path = "kfz-angelegenheiten"
calendar_uid = "bffa082e-0640-40b4-ad60-4a98355850ff"
service_uid = "03ae67e1-9222-4c34-bc52-f4037b54878c"
ntfy_topic = "termin-checker-ci-smoke-test"
deadline = "$deadline"
EOF

echo "--- check"
"$bin" check || echo "::warning::live check failed, booking site not reachable from CI"

echo "--- web"
"$bin" web > web.out 2>&1 &
web_pid=$!
for _ in $(seq 30); do grep -q "http://" web.out && break; sleep 1; done
url=$(grep -o 'http://[0-9.:]*' web.out)
curl -fsS "$url/" | grep -q "Letzte Suche fortsetzen"
test "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$url/check")" = 403
test "$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.example' "$url/")" = 403
kill "$web_pid" || true

if [ "${RUNNER_OS:-}" = Linux ]; then
  echo "--- service skipped, no systemd user session on the runner"
  exit 0
fi

echo "--- service"
"$bin" install
for _ in $(seq 30); do "$bin" status | grep -q "Watcher: aktiv" && break; sleep 2; done
"$bin" status
"$bin" status | grep -q "Watcher: aktiv"
for _ in $(seq 30); do grep -q "Provider: bochum" "$cfg_dir/watcher.log" 2>/dev/null && break; sleep 2; done
cat "$cfg_dir/watcher.log"
grep -q "Provider: bochum" "$cfg_dir/watcher.log"
"$bin" stop
"$bin" status | grep -q "Watcher: nicht installiert"
echo "--- ok"
