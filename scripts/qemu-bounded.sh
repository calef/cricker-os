#!/bin/sh
#
# Run a command with a hard time limit, and actually kill it.
#
#     scripts/qemu-bounded.sh 10 qemu-system-aarch64 -machine virt ...
#
# # Why this exists
#
# The obvious trick, `perl -e 'alarm N; exec @ARGV' <cmd>`, **does not work on QEMU.**
# QEMU installs its own SIGALRM handler (it uses timers internally), so the alarm is
# swallowed and the process runs forever.
#
# We found this out the hard way: eleven abandoned QEMU processes accumulated over a day
# of development, burning a combined 729% CPU, the oldest with almost eight hours of CPU
# time on it. Every "bounded" run had leaked.
#
# QEMU *does* honour SIGTERM, so that's what we use: start the child, start a killer in
# the background, and make sure the killer dies with us.
#
# Note the `<&0` on the child: a backgrounded command's stdin is otherwise redirected to
# /dev/null by the shell (POSIX), which silently breaks piping input to QEMU's serial port.
# We found that the hard way trying to drive the milestone-10 shell from a pipe.

set -e

SECONDS_LIMIT="$1"
shift

"$@" <&0 &
CHILD=$!

# The killer. Detached, so it survives even if the shell is in a pipeline whose reader
# (`head`, say) exits early. That exact case is what leaked processes before.
(
    sleep "$SECONDS_LIMIT"
    kill -TERM "$CHILD" 2>/dev/null || true
    sleep 2
    kill -KILL "$CHILD" 2>/dev/null || true
) &
KILLER=$!

set +e
wait "$CHILD"
STATUS=$?
set -e

# Don't leave the killer sleeping if the child finished on its own.
kill "$KILLER" 2>/dev/null || true

exit "$STATUS"
