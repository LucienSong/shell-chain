#!/bin/bash
set -e

DATADIR="${DATADIR:-/data}"
SHARED="${SHARED:-/shared}"

mkdir -p "$DATADIR" "$SHARED"

if [ "$GENESIS_CREATOR" = "true" ]; then
    # First node: start shell-node which auto-creates genesis.json,
    # then copy it to the shared volume for other nodes.
    shell-node run "$@" &
    NODE_PID=$!

    # Wait for genesis.json to be written by the node during startup.
    for i in $(seq 1 30); do
        if [ -f "$DATADIR/genesis.json" ]; then
            cp "$DATADIR/genesis.json" "$SHARED/genesis.json"
            echo "✓ Genesis shared to $SHARED/genesis.json"
            break
        fi
        sleep 0.5
    done

    wait $NODE_PID
else
    # Follower node: wait for shared genesis before starting.
    echo "Waiting for genesis.json from leader..."
    for i in $(seq 1 60); do
        if [ -f "$SHARED/genesis.json" ]; then
            cp "$SHARED/genesis.json" "$DATADIR/genesis.json"
            echo "✓ Genesis loaded from shared volume"
            break
        fi
        sleep 1
    done

    if [ ! -f "$DATADIR/genesis.json" ]; then
        echo "ERROR: genesis.json not found after 60s"
        exit 1
    fi

    exec shell-node run "$@"
fi
