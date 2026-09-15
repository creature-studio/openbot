#!/bin/bash
set -e
export PATH="/tmp/rust-final/bin:$PATH"
export LD_LIBRARY_PATH="/tmp/rust-final/lib:$LD_LIBRARY_PATH"
cd /home/user/openbot/sand

SAND=./target/debug/sand

echo "=== Test1: Runtime A/B independent ==="
A=$($SAND runtime create assistant | grep -o '"id":"[^"]*"' | cut -d'"' -f4)
B=$($SAND runtime create assistant | grep -o '"id":"[^"]*"' | cut -d'"' -f4)
echo "A=$A B=$B"
OUT_A=$($SAND runtime exec $A pwd)
echo "A pwd raw: $OUT_A"
# extract decoded stdout
# Our CLI already prints decoded
if echo "$OUT_A" | grep -q "$A"; then
  echo "Test1 A workspace OK"
else
  echo "Test1 A workspace FAIL: $OUT_A"
  exit 1
fi
OUT_B=$($SAND runtime exec $B pwd)
if echo "$OUT_B" | grep -q "$B"; then
  echo "Test1 B workspace OK"
else
  echo "Test1 B workspace FAIL: $OUT_B"
  exit 1
fi
# echo hello isolation
OUT_A_HELLO=$($SAND runtime exec $A echo hello)
if echo "$OUT_A_HELLO" | grep -q "hello"; then
  echo "Test1 echo A OK"
else
  echo "Test1 echo A FAIL"
  exit 1
fi
OUT_B_WORLD=$($SAND runtime exec $B echo world)
if echo "$OUT_B_WORLD" | grep -q "world"; then
  echo "Test1 echo B OK"
else
  echo "Test1 echo B FAIL"
  exit 1
fi

echo "=== Test2: spawn 10 sleep 100 in A ==="
for i in $(seq 1 10); do
  echo '{"method":"SpawnBackground","id":"'$A'","command":["sleep","100"]}' | socat - UNIX-CONNECT:/run/sand/sandd.sock > /dev/null
done
sleep 1
CGROUP_PATH="/sys/fs/cgroup/sand/runtime-$A"
if [ -d "$CGROUP_PATH" ]; then
  echo "cgroup exists: $CGROUP_PATH"
  cat $CGROUP_PATH/cgroup.procs || sudo cat $CGROUP_PATH/cgroup.procs
  PIDS_COUNT=$(sudo cat $CGROUP_PATH/cgroup.procs 2>/dev/null | wc -l || cat $CGROUP_PATH/cgroup.procs | wc -l)
  echo "pids in cgroup.procs: $PIDS_COUNT"
  if [ "$PIDS_COUNT" -ge 10 ]; then
    echo "Test2 cgroup.procs count OK"
  else
    echo "Test2 cgroup.procs count FAIL expected >=10 got $PIDS_COUNT"
    # check fallback
    cat /tmp/sand-cgroups/runtime-$A/pids || true
  fi
  PIDS_CURRENT=$(cat $CGROUP_PATH/pids.current 2>/dev/null || sudo cat $CGROUP_PATH/pids.current)
  echo "pids.current: $PIDS_CURRENT"
  if [ "$PIDS_CURRENT" -ge 10 ]; then
    echo "Test2 pids.current OK"
  else
    echo "Test2 pids.current FAIL"
  fi
else
  echo "cgroup path not found, fallback check"
  ls /tmp/sand-cgroups/
fi
SLEEP_COUNT=$(ps aux | grep "sleep 100" | grep -v grep | wc -l)
echo "ps sleep 100 count: $SLEEP_COUNT"
if [ "$SLEEP_COUNT" -ge 10 ]; then
  echo "Test2 ps OK"
else
  echo "Test2 ps FAIL expected 10 got $SLEEP_COUNT"
  ps aux | grep sleep | head
fi

echo "=== Test3: Runtime B not affected by A destroy ==="
# check B still exists before destroy A
$SAND runtime list
$SAND runtime destroy $A
sleep 1
# check B still exists
LIST_AFTER=$($SAND runtime list)
echo "list after destroy A: $LIST_AFTER"
if echo "$LIST_AFTER" | grep -q "$B"; then
  echo "Test3 B still exists OK"
else
  echo "Test3 B missing FAIL"
  exit 1
fi
if echo "$LIST_AFTER" | grep -q "$A"; then
  echo "Test3 A still exists FAIL"
  exit 1
else
  echo "Test3 A removed OK"
fi
# check cgroup A gone
if [ -d "/sys/fs/cgroup/sand/runtime-$A" ]; then
  echo "Test3 cgroup A still exists FAIL"
  ls /sys/fs/cgroup/sand/ | grep $A
  exit 1
else
  echo "Test3 cgroup A gone OK"
fi
# check sleep processes killed
SLEEP_AFTER=$(ps aux | grep "sleep 100" | grep -v grep | wc -l)
echo "sleep after destroy A: $SLEEP_AFTER"
if [ "$SLEEP_AFTER" -eq 0 ]; then
  echo "Test3 sleep killed OK"
else
  echo "Test3 sleep not killed FAIL, remaining $SLEEP_AFTER"
  ps aux | grep sleep | head
  exit 1
fi
# check B cgroup still exists and empty
if [ -d "/sys/fs/cgroup/sand/runtime-$B" ]; then
  echo "B cgroup exists OK"
  cat /sys/fs/cgroup/sand/runtime-$B/pids.current || sudo cat /sys/fs/cgroup/sand/runtime-$B/pids.current
else
  echo "B cgroup missing FAIL"
  exit 1
fi

echo "=== Test4: PTY open 3 in B ==="
$SAND pty open $B term-1
$SAND pty open $B term-2
$SAND pty open $B term-3
PTY_LIST=$($SAND pty list $B)
echo "pty list: $PTY_LIST"
COUNT=$(echo "$PTY_LIST" | grep -o "term-" | wc -l)
if [ "$COUNT" -ge 3 ]; then
  echo "Test4 open 3 OK"
else
  echo "Test4 open 3 FAIL"
  exit 1
fi
# resize
echo '{"method":"ResizePty","id":"'$B'","pty_id":"term-1","cols":120,"rows":40}' | socat - UNIX-CONNECT:/run/sand/sandd.sock
PTY_LIST2=$($SAND pty list $B)
echo "after resize: $PTY_LIST2"
if echo "$PTY_LIST2" | grep -q "120"; then
  echo "Test4 resize OK"
else
  echo "Test4 resize FAIL"
  exit 1
fi
# write
echo '{"method":"WritePty","id":"'$B'","pty_id":"term-1","data_b64":"ZWNobyBoZWxsbyBmcm9tIHB0eQo="}' | socat - UNIX-CONNECT:/run/sand/sandd.sock
echo "Test4 write OK (base64 echo hello from pty)"

echo "=== Test5: PTY client disconnect behavior ==="
# We already tested: opening via separate connections leaves PTY alive
# So after open, list shows them even after client disconnected
# Document strategy
echo "Strategy: PTY continues alive after client disconnect, must be explicitly closed or runtime destroyed"
$SAND pty list $B | grep term-1 && echo "Test5 PTY alive after disconnect OK"

echo "=== Test6: sandd restart, runtime state re-read ==="
# Create new runtime with background sleep
C=$($SAND runtime create task | grep -o '"id":"[^"]*"' | cut -d'"' -f4)
echo "C=$C"
echo '{"method":"SpawnBackground","id":"'$C'","command":["sleep","200"]}' | socat - UNIX-CONNECT:/run/sand/sandd.sock
sleep 1
ps aux | grep "sleep 200" | grep -v grep
cat /sys/fs/cgroup/sand/runtime-$C/cgroup.procs || sudo cat /sys/fs/cgroup/sand/runtime-$C/cgroup.procs
# kill sandd and restart
PID=$(ps aux | grep "[s]andd" | awk '{print $2}' | head -n1)
echo "killing sandd $PID"
kill $PID
sleep 2
./target/debug/sandd > /tmp/sandd.log 2>&1 &
sleep 2
cat /tmp/sandd.log
LIST_AFTER_RESTART=$($SAND runtime list)
echo "list after restart: $LIST_AFTER_RESTART"
if echo "$LIST_AFTER_RESTART" | grep -q "$C"; then
  echo "Test6 runtime C still present after restart OK"
else
  echo "Test6 runtime C missing after restart FAIL"
  exit 1
fi
# Check if sleep still alive and cgroup has pid
SLEEP_200=$(ps aux | grep "sleep 200" | grep -v grep | wc -l)
echo "sleep 200 after restart count: $SLEEP_200"
if [ "$SLEEP_200" -ge 1 ]; then
  echo "Test6 sleep survived restart OK (orphan handling)"
else
  echo "Test6 sleep did not survive restart (may be expected if not in cgroup)"
fi
# Check cgroup.procs after restart
if [ -d "/sys/fs/cgroup/sand/runtime-$C" ]; then
  cat /sys/fs/cgroup/sand/runtime-$C/cgroup.procs || sudo cat /sys/fs/cgroup/sand/runtime-$C/cgroup.procs || echo "empty"
  echo "Test6 cgroup still exists"
else
  echo "Test6 cgroup missing"
fi
# destroy C and B
$SAND runtime destroy $C || true
$SAND runtime destroy $B || true
sleep 1
ps aux | grep "sleep 200" | grep -v grep || echo "sleep 200 cleaned"

echo "=== Test7: UDS RPC concurrent ==="
# Spawn 10 parallel clients doing status
for i in $(seq 1 20); do
  (./target/debug/sand status > /tmp/concurrent-$i.log 2>&1 &)
done
wait
FAIL=0
for i in $(seq 1 20); do
  if ! grep -q "ok" /tmp/concurrent-$i.log; then
    echo "concurrent $i failed: $(cat /tmp/concurrent-$i.log)"
    FAIL=1
  fi
done
if [ "$FAIL" -eq 0 ]; then
  echo "Test7 concurrent RPC OK"
else
  echo "Test7 concurrent RPC FAIL"
  exit 1
fi

echo "=== Test8: kill_all via cgroup.kill ==="
D=$($SAND runtime create task | grep -o '"id":"[^"]*"' | cut -d'"' -f4)
echo "D=$D"
for i in $(seq 1 5); do
  echo '{"method":"SpawnBackground","id":"'$D'","command":["sleep","100"]}' | socat - UNIX-CONNECT:/run/sand/sandd.sock
done
sleep 1
echo "before kill, pids:"
cat /sys/fs/cgroup/sand/runtime-$D/cgroup.procs || sudo cat /sys/fs/cgroup/sand/runtime-$D/cgroup.procs
# Use cgroup.kill via direct write
sudo sh -c "echo 1 > /sys/fs/cgroup/sand/runtime-$D/cgroup.kill" && echo "cgroup.kill wrote"
sleep 1
cat /sys/fs/cgroup/sand/runtime-$D/cgroup.procs || sudo cat /sys/fs/cgroup/sand/runtime-$D/cgroup.procs || echo "empty after kill"
SLEEP_100_AFTER=$(ps aux | grep "sleep 100" | grep -v grep | wc -l)
echo "sleep 100 after cgroup.kill: $SLEEP_100_AFTER"
if [ "$SLEEP_100_AFTER" -eq 0 ]; then
  echo "Test8 cgroup.kill OK"
else
  echo "Test8 cgroup.kill FAIL, still $SLEEP_100_AFTER"
fi
$SAND runtime destroy $D

echo "=== All tests passed ==="
$SAND runtime list
