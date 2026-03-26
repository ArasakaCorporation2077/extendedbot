#!/bin/bash
# Auto-restart wrapper for extended-mm bot
cd "$(dirname "$0")"

while true; do
    echo "$(date) Starting extended-mm..."
    ./target/release/extended-mm >> ~/bot.log 2>&1
    EXIT_CODE=$?
    echo "$(date) Bot exited with code $EXIT_CODE, restarting in 5s..."
    sleep 5
done
