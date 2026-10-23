#!/bin/bash
# A Linux node run for real: `horadric node` with a shell in place of the
# agent, a fake session posted to its port, a shell session started
# through `horadric new`, both read over `horadric node --stdio`. Then the
# node is killed and started again, to see the session's host outlive it
# and the new node attach to it. Logs are left in $OUT.
set -u
BIN="${BIN:-target/debug/horadric}"
OUT="${OUT:-$PWD/linux-smoke}"
PORT=43118
export HORADRIC_DEV=1
export HORADRIC_AGENT=/bin/bash
rm -rf "$OUT" "$HOME/.local/state/Horadric-dev"
mkdir -p "$OUT"
failed=0
fail() { echo "FAIL: $*"; failed=1; }

up() {
  for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then return 0; fi
    sleep 0.2
  done
  return 1
}

start_node() {
  "$BIN" node >"$OUT/node-$1.log" 2>&1 &
  NODE=$!
  up || fail "the node did not listen on $PORT ($1)"
}

stop_all() {
  kill "$NODE" 2>/dev/null
  pkill -f "$BIN host" 2>/dev/null
}
trap stop_all EXIT

post() { # id event cwd [extra json]
  curl -s -m 3 -o /dev/null -X POST "http://127.0.0.1:$PORT/horadric/hook" \
    -H "X-Horadric-Session: $1" -H 'Content-Type: application/json' \
    -d "{\"session_id\":\"\",\"hook_event_name\":\"$2\",\"cwd\":\"$3\",\"name\":\"$1\"$4}"
}

start_node first
post fake HoradricRegister /tmp ''
post fake UserPromptSubmit /tmp ',"prompt":"fix the build"'
"$BIN" new --name shell --cwd "$PWD" -- --norc --noprofile -i \
  >"$OUT/new.log" 2>&1 || fail "horadric new: $(cat "$OUT/new.log")"

python3 .github/linux-smoke.py "$BIN" first >"$OUT/stdio-first.log" 2>&1 ||
  fail "over --stdio: $(tail -5 "$OUT/stdio-first.log")"

kill "$NODE"
wait "$NODE" 2>/dev/null
pgrep -f "$BIN host" >/dev/null || fail "the session's host did not outlive the node"
start_node again
# The fake session posts again; a node keeps no state across a restart.
post fake HoradricRegister /tmp ''
python3 .github/linux-smoke.py "$BIN" again >"$OUT/stdio-again.log" 2>&1 ||
  fail "over --stdio after a restart: $(tail -5 "$OUT/stdio-again.log")"

for f in "$OUT"/*.log; do
  echo "== $f"
  cat "$f"
done
if [ "$failed" = 0 ]; then echo "linux smoke: all passed"; fi
exit "$failed"
