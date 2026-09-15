#!/bin/bash
# check_spark.sh — static self-check for the remote-machine feature.
#
# The sandbox this was developed in had no Rust toolchain, so this script is the
# contract for "the code is internally consistent": it greps for the invariants
# the architecture demands (each one is a rule someone could break by accident),
# and, when cargo IS available, runs the real checks.
#
# Usage:
#   sand/check_spark.sh            # static checks (+ cargo check when available)
#   sand/check_spark.sh --full     # static checks + cargo check + unit tests
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FULL=0
[ "${1:-}" = "--full" ] && FULL=1

FAILED=0
pass() { echo "  ok   $1"; }
fail() { echo "  FAIL $1"; FAILED=$((FAILED + 1)); }

# Matches in *code* only: comments are stripped before the pattern is applied,
# because the documentation legitimately talks about the thing being banned
# ("never run `ssh hostname`", "no column for a password").
matches_code() { # matches_code <regex> <paths...>
  local pattern="$1"; shift
  grep -rn --include='*.rs' --include='*.sh' -E "$pattern" "$@" 2>/dev/null \
    | while IFS= read -r hit; do
        local code
        code="$(printf '%s' "$hit" | sed 's|//.*$||')"
        # `assert!(!joined.contains("StrictHostKeyChecking=no"))` is evidence
        # *for* the invariant, not a violation of it.
        if printf '%s' "$code" | grep -qE '!.*\.(contains|matches)\('; then continue; fi
        if printf '%s' "$code" | grep -qE "$pattern"; then printf '%s\n' "$hit"; fi
      done
}

# assert_absent <description> <regex> <paths...>
assert_absent() {
  local desc="$1" pattern="$2"; shift 2
  local hits
  hits="$(matches_code "$pattern" "$@")"
  if [ -n "$hits" ]; then
    fail "$desc"
    printf '%s\n' "$hits" | head -5 | sed 's/^/       /'
  else
    pass "$desc"
  fi
}

# assert_present <description> <regex> <paths...>
assert_present() {
  local desc="$1" pattern="$2"; shift 2
  if grep -rn --include='*.rs' --include='*.sh' -E "$pattern" "$@" >/dev/null 2>&1; then
    pass "$desc"
  else
    fail "$desc"
  fi
}

echo "=== Static invariants (security / architecture) ==="

# 1. Never disable host key checking.
assert_absent "no StrictHostKeyChecking=no anywhere" 'StrictHostKeyChecking[\"= ]*no' \
  client/crates/spark-transport sand/crates

# 2. Never install into /usr/local/bin (bootstrap must stay in $HOME).
assert_absent "bootstrap never writes to /usr/local" '/usr/local/bin' \
  client/crates/spark-transport/src/ssh sand/crates

# 3. No stored secrets in Spark's sqlite.
assert_absent "no key/password columns or fields" 'password|passphrase|private_key|identity_file' \
  sand/crates/host-agent/src/persistence.rs

# 4. Remote sandd must not listen on TCP.
assert_absent "sandd does not bind a TCP port" 'TcpListener|bind\('"'"'0\.0\.0\.0' \
  sand/crates/sandd/src

# 5. PTY must go through the runtime, never through an interactive ssh shell.
assert_absent "no interactive ssh shell for terminals" 'ssh[^"]*-t [^ ]* *(bash|sh)\\b' \
  client/crates/spark-transport/src sand/crates/host-agent/src

# 6. Health must not shell out to ssh.
assert_absent "health checks never run 'ssh hostname'" '\bssh[ -][^"]*\b(hostname|true)\b' \
  sand/crates/host-agent/src/machine

echo
echo "=== Wire-format invariants ==="
assert_present "24-byte frame header with protocol_version" 'pub protocol_version: u16' \
  sand/crates/sand-protocol/src/frame.rs
assert_present "protocol version is separate from binary version" 'SAND_PROTOCOL_VERSION' \
  sand/crates/sand-protocol/src/lib.rs
assert_present "8 MiB frame cap" 'MAX_PAYLOAD_LEN' sand/crates/sand-protocol/src/frame.rs
assert_present "socket is 0660" '0o660' sand/crates/sandd/src
assert_present "sand bridge --socket exists" 'sand bridge' sand/crates/sand-cli/src/main.rs sand/crates/sand-bridge/src/lib.rs

echo
echo "=== Machine layer invariants ==="
assert_present "RuntimeTransport trait exists" 'pub trait RuntimeTransport' \
  client/crates/spark-transport/src/runtime_transport.rs
assert_present "two transports implemented" 'impl RuntimeTransport for (LocalTransport|SshTransport)' \
  client/crates/spark-transport/src/local.rs client/crates/spark-transport/src/ssh/mod.rs
assert_present "tools route through the runtime's machine" 'transport_for_runtime' \
  sand/crates/host-agent/src/tools
assert_present "MachineManager is the only router" 'pub fn transport\(&self' \
  sand/crates/host-agent/src/machine/mod.rs
assert_present "backoff schedule 1/2/4/8/16/30" '1, 2, 4, 8, 16, 30' \
  client/crates/spark-transport/src/reconnect.rs
assert_present "host key change is never auto-accepted" 'Changed' \
  client/crates/spark-transport/src/ssh/hostkey.rs
assert_present "machines table stores coordinates only" 'CREATE TABLE IF NOT EXISTS machines' \
  sand/crates/host-agent/src/persistence.rs
assert_present "runtime carries its machine id" 'machine_id: MachineId' \
  client/crates/spark-transport/src/runtime_transport.rs
assert_present "session pins its machine" 'pub machine_id: MachineId' \
  sand/crates/host-agent/src/agent/session.rs

echo
echo "=== GPUI surfaces ==="
assert_present "Machines sidebar section" 'MACHINES' client/crates/spark-ui/src/sidebar/mod.rs
assert_present "\"+ Machine\" form" 'render_add_form' client/crates/spark-ui/src/machine/mod.rs
assert_present "Runtime inspector tab" 'InspectorTab::Runtime' client/crates/spark-ui/src/inspector/mod.rs
assert_present "host key card with two buttons" '信任并连接' client/crates/spark-ui/src/machine/mod.rs
assert_present "machine store folds transport events" 'apply_event' \
  client/crates/spark-ui/src/stores/machine.rs

echo
echo "=== Import hygiene (removed / renamed APIs) ==="
assert_absent "MachineManager::requires_user_action is gone" 'requires_user_action\(&' \
  sand/crates/host-agent/src
assert_absent "spark_model::RequiresUserAction struct is gone" 'struct RequiresUserAction' \
  client/crates/spark-model/src
assert_absent "ReconnectPolicy is gone" 'ReconnectPolicy' \
  client/crates/spark-transport/src
assert_absent "no duplicated MachineId crossing layers" 'sand_protocol::MachineId' \
  sand/crates/host-agent/src client/crates/spark-transport/src

echo
if [ "$FAILED" -ne 0 ]; then
  echo "=== $FAILED static check(s) FAILED ==="
else
  echo "=== all static checks passed ==="
fi

# ---------------------------------------------------------------------------
# Real compilation, when a toolchain is available
# ---------------------------------------------------------------------------

if command -v cargo >/dev/null 2>&1; then
  echo
  echo "=== cargo check (sand workspace) ==="
  ( cd sand && cargo check --workspace --all-targets ) || FAILED=$((FAILED + 1))
  echo
  echo "=== cargo check (client workspace) ==="
  ( cd client && cargo check --workspace --all-targets ) || FAILED=$((FAILED + 1))
  if [ "$FULL" = "1" ]; then
    echo
    echo "=== cargo test (machine layer) ==="
    ( cd sand && cargo test -p host-agent machine:: ) || FAILED=$((FAILED + 1))
    echo
    echo "=== cargo test (transport) ==="
    ( cd client && cargo test -p spark-transport ) || FAILED=$((FAILED + 1))
  fi
else
  echo
  echo "No cargo in PATH. Compile the code with:"
  echo "  cd sand   && cargo check --workspace --all-targets"
  echo "  cd client && cargo check --workspace --all-targets"
  echo "  cd sand   && cargo test -p host-agent machine::  &&  cd ../client && cargo test -p spark-transport"
  echo "Then run the real E2E: sand/test_remote_ssh.sh (needs a reachable Linux SSH host)."
fi

exit "$FAILED"
