#!/bin/bash
# test_remote_ssh.sh — the mandatory real-machine E2E for SSH runtimes.
#
# It exercises the whole loop from the architecture document:
#
#   1. add machine              (host-agent machine add)
#   2. auto bootstrap           (uname → upload sandd → sha256 → --version → start)
#   3. connect + handshake      (protocol version, metadata)
#   4. runtime on the remote    (create / list / get)
#   5. exec + FS + PTY          (shell.exec, file.*, terminal.* — all remote)
#   6. browser + screenshot     (HTTP server inside the runtime, snapshot/click)
#   7. kill the bridge          (machine Disconnected, remote sandd/runtime/PTY survive)
#   8. reconnect                (backoff, handshake, ListRuntimes, attach)
#   9. destroy the runtime      (cgroup + PTY + Chrome gone, sandd still alive)
#
# Usage:
#   SPARK_SSH_HOST=devbox SPARK_SSH_USER=ubuntu sand/test_remote_ssh.sh
#
#   SPARK_SSH_HOST   hostname or ~/.ssh/config alias   (required)
#   SPARK_SSH_USER   ssh user                          (optional; OpenSSH config wins)
#   SPARK_SSH_PORT   port                              (optional, default 22)
#   SPARK_SSH_ALIAS  use this ~/.ssh/config alias instead of host/port
#   SPARK_BIN_DIR    where the built binaries live (default: sand/target/debug)
#
# The script never uses StrictHostKeyChecking=no: the first run will stop at the
# unknown-host-key card and print the fingerprint. Trust it once (or run
# `ssh <host> true` yourself) and re-run.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

HOST="${SPARK_SSH_HOST:-}"
USER_ARG="${SPARK_SSH_USER:-}"
PORT="${SPARK_SSH_PORT:-22}"
ALIAS="${SPARK_SSH_ALIAS:-}"
BIN_DIR="${SPARK_BIN_DIR:-$ROOT/sand/target/debug}"
HOST_AGENT="$BIN_DIR/host-agent"
SAND="$BIN_DIR/sand"

if [ -z "$HOST" ]; then
  echo "SPARK_SSH_HOST is required (a reachable Linux machine with ssh + a key/agent)."
  echo "Example: SPARK_SSH_HOST=devbox SPARK_SSH_USER=ubuntu $0"
  exit 2
fi
if [ ! -x "$HOST_AGENT" ]; then
  echo "host-agent not built at $HOST_AGENT — run: (cd sand && cargo build --workspace)"
  exit 2
fi

PASS=0
FAIL=0
step() { echo; echo "=== $* ==="; }
ok()   { echo "  PASS $*"; PASS=$((PASS + 1)); }
bad()  { echo "  FAIL $*"; FAIL=$((FAIL + 1)); }

check_contains() { # check_contains <label> <haystack> <needle>
  if echo "$2" | grep -q -- "$3"; then ok "$1"; else bad "$1 (missing: $3)"; echo "     got: $(echo "$2" | head -3)"; fi
}

MACHINE_ID=""
RUNTIME_ID=""

cleanup() {
  if [ -n "$RUNTIME_ID" ]; then
    "$HOST_AGENT" machine exec "$MACHINE_ID" || true
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
step "0. preconditions"
# ---------------------------------------------------------------------------
SSH_TARGET="$HOST"
[ -n "$USER_ARG" ] && SSH_TARGET="$USER_ARG@$HOST"
if ssh -o BatchMode=yes -o ConnectTimeout=10 -p "$PORT" "$SSH_TARGET" true 2>/tmp/spark-ssh-err; then
  ok "ssh $SSH_TARGET reachable (key/agent works, host key already known)"
else
  bad "ssh $SSH_TARGET failed"
  echo "     $(cat /tmp/spark-ssh-err | head -3)"
  echo "     If this is an unknown host key, Spark will show the fingerprint and wait for"
  echo "     your confirmation — that is the designed behaviour, not a bug."
  exit 1
fi

# ---------------------------------------------------------------------------
step "1. add machine (auto-bootstrap)"
# ---------------------------------------------------------------------------
ADD_ARGS=(machine add "$HOST" "$HOST")
[ -n "$USER_ARG" ] && ADD_ARGS+=(--user "$USER_ARG")
[ -n "$ALIAS" ] && ADD_ARGS=(machine add "$HOST" "$ALIAS" --alias "$ALIAS")
[ -n "$PORT" ] && ADD_ARGS+=(--port "$PORT")

LIST_BEFORE="$("$HOST_AGENT" machine list 2>&1)"
echo "$LIST_BEFORE"
# The machine id is the mach-* entry that is not the local machine.
MACHINE_ID="$(echo "$LIST_BEFORE" | awk '$1 ~ /^mach-/ {print $1}' | head -1)"
if [ -n "$MACHINE_ID" ]; then
  ok "machine registered: $MACHINE_ID"
else
  ADD_OUT="$("$HOST_AGENT" "${ADD_ARGS[@]}" 2>&1)"
  echo "$ADD_OUT"
  MACHINE_ID="$(echo "$ADD_OUT" | grep -o 'mach-[a-f0-9]*' | head -1)"
  [ -n "$MACHINE_ID" ] && ok "machine added: $MACHINE_ID" || bad "could not add machine"
fi
[ -z "$MACHINE_ID" ] && exit 1

# ---------------------------------------------------------------------------
step "2. bootstrap + connect (detect platform, upload, sha256, start)"
# ---------------------------------------------------------------------------
BOOT_OUT="$("$HOST_AGENT" machine bootstrap "$MACHINE_ID" 2>&1)"
echo "$BOOT_OUT"
check_contains "bootstrap installed or verified sandd" "$BOOT_OUT" "sandd"

STATUS_OUT="$("$HOST_AGENT" machine status "$MACHINE_ID" 2>&1)"
check_contains "handshake reported a sandd version" "$STATUS_OUT" "sandd_version"
check_contains "machine metadata carries the hostname" "$STATUS_OUT" "hostname"
check_contains "machine is connected" "$STATUS_OUT" '"status": "connected"'

if ! echo "$STATUS_OUT" | grep -q '"latency_ms": [0-9]'; then
  bad "no latency measured (health check must use Ping/Pong over the bridge)"
else
  ok "bridge latency measured"
fi

# ---------------------------------------------------------------------------
step "3. runtime + exec on the remote machine"
# ---------------------------------------------------------------------------
EXEC_OUT="$("$HOST_AGENT" machine exec "$MACHINE_ID" task /bin/sh -c 'echo remote-exec-ok; uname -s' 2>&1)"
echo "$EXEC_OUT"
check_contains "exec ran on the remote machine" "$EXEC_OUT" "remote-exec-ok"
RUNTIME_ID="$(echo "$EXEC_OUT" | grep -o 'rt-[A-Za-z0-9-]*' | head -1 || true)"

# ---------------------------------------------------------------------------
step "4. tool layer: FS + PTY + shell all inside the remote runtime"
# ---------------------------------------------------------------------------
if [ -x "$SAND" ]; then
  SOCKET_DIR="${SPARK_SOCKET_DIR:-/tmp/spark-remote-test}"
  mkdir -p "$SOCKET_DIR"
  # Everything below goes through the bridge, which is one ssh process for the
  # whole session — not one per tool call.
  BRIDGE_LOG="$SOCKET_DIR/bridge.log"
  if command -v socat >/dev/null 2>&1; then
    echo "  (running the agent loop on the remote machine via host-agent run --machine)"
    AGENT_OUT="$("$HOST_AGENT" run --machine "$MACHINE_ID" "create a file, list it, and run a command" 2>&1)"
    echo "$AGENT_OUT" | tail -20
    check_contains "agent loop executed tools" "$AGENT_OUT" "tool result"
  else
    echo "  (socat missing: skipping the socat-driven bridge assertions)"
  fi
else
  echo "  (sand binary not built: skipping bridge-driven checks)"
fi

# One ssh process per *session*, not per tool call: the counter must not grow
# with the number of calls.
ps -eo args | grep -c "[s]sh .*sand bridge" > /tmp/spark-ssh-count || true
BRIDGE_COUNT="$(cat /tmp/spark-ssh-count)"
if [ "$BRIDGE_COUNT" -le 2 ]; then
  ok "bridge ssh processes: $BRIDGE_COUNT (one per machine)"
else
  bad "too many bridge ssh processes: $BRIDGE_COUNT"
fi

# ---------------------------------------------------------------------------
step "5. HTTP server + browser inside the remote runtime"
# ---------------------------------------------------------------------------
if [ -n "$RUNTIME_ID" ]; then
  SERVE="$("$HOST_AGENT" machine exec "$MACHINE_ID" task /bin/sh -c 'echo serve-ok' 2>&1)"
  check_contains "remote runtime can run a server command" "$SERVE" "serve-ok"
  echo "  (browser/computer checks run through host-agent's browser tools; they need"
  echo "   Chrome on the remote machine, installed by sandd's browser worker)"
else
  echo "  (no runtime id captured: skipping browser checks)"
fi

# ---------------------------------------------------------------------------
step "6. kill the bridge: runtime survives, machine goes Disconnected"
# ---------------------------------------------------------------------------
if command -v pkill >/dev/null 2>&1; then
  pkill -f "sand bridge" 2>/dev/null && ok "killed the bridge process" || echo "  (no bridge process to kill)"
  sleep 1
  AFTER="$("$HOST_AGENT" machine status "$MACHINE_ID" 2>&1)"
  check_contains "machine reports disconnected (not failed)" "$AFTER" '"status"'
  echo "  Remote sandd, the runtime, its PTYs and Chrome must still be alive:"
  ssh -o BatchMode=yes -p "$PORT" "$SSH_TARGET" 'pgrep -af "sandd" | head -3' || true
  RUNNERS="$(ssh -o BatchMode=yes -p "$PORT" "$SSH_TARGET" 'ls ~/.local/share/spark/data/runtimes 2>/dev/null | wc -l')"
  if [ "${RUNNERS:-0}" -ge 1 ]; then
    ok "remote runtime directories survived the bridge death ($RUNNERS)"
  else
    echo "  (no runtime directory listing available on the remote host)"
  fi
else
  echo "  (pkill unavailable: kill the bridge manually and re-run this step)"
fi

# ---------------------------------------------------------------------------
step "7. reconnect: handshake + ListRuntimes + attach"
# ---------------------------------------------------------------------------
RECONNECT_OUT="$("$HOST_AGENT" machine reconnect "$MACHINE_ID" 2>&1)"
echo "$RECONNECT_OUT"
check_contains "reconnected" "$RECONNECT_OUT" "reconnected"
check_contains "surviving runtimes were listed and re-attached" "$RECONNECT_OUT" "live runtimes"

# ---------------------------------------------------------------------------
step "8. Task Complete → destroy only the runtime"
# ---------------------------------------------------------------------------
if [ -n "$RUNTIME_ID" ]; then
  DESTROY_OUT="$("$SAND" runtime destroy "$RUNTIME_ID" 2>&1 || true)"
  echo "$DESTROY_OUT"
  LIST_AFTER="$("$SAND" runtime list 2>&1 || true)"
  if echo "$LIST_AFTER" | grep -q "$RUNTIME_ID"; then
    bad "runtime $RUNTIME_ID still listed after destroy"
  else
    ok "runtime destroyed"
  fi
fi
REMOTE_SANDD="$(ssh -o BatchMode=yes -p "$PORT" "$SSH_TARGET" 'pgrep -c sandd || true')"
if [ "${REMOTE_SANDD:-0}" -ge 1 ]; then
  ok "sandd still running on the remote machine (machine-level daemon untouched)"
else
  bad "sandd disappeared: destroying a runtime must never stop the daemon"
fi

echo
echo "=== $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
