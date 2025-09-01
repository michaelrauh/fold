#!/bin/bash
set -euo pipefail

# Build once
echo "Building..."
cargo build

BIN="target/debug/fold"
FIFO="/tmp/fold-cmds.$$"
rm -f "$FIFO"
mkfifo "$FIFO"

# Start the REPL reading from the FIFO
"$BIN" < "$FIFO" &
FOLD_PID=$!
echo "Started fold (pid=$FOLD_PID); sending commands via $FIFO"

send() {
	echo "$@" > "$FIFO"
}

# Workflow (commands are sent to the running REPL)
send list-files
send stage-file e.txt
send ingest-file-split e.txt CHAPTER
send list-files
send clean-files-small 100
send list-files
send feed-file e.txt-part-56
send database
send queues
send interner-versions

sleep 10

send feed-file e.txt-part-55
send database
send queues

echo "scale-worker: skipped in local mode"
sleep 10

send database
send queues
send print-optimal

# Gracefully exit the REPL
send exit

# Wait for process to finish and cleanup
wait "$FOLD_PID"
rm -f "$FIFO"

echo "Workflow complete."