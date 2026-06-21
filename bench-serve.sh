#!/bin/bash
# Benchmark serving speed of a ZIM archive.
# Usage: ./bench-serve.sh <file.zim> [concurrency]
#
# Finds the main page, extracts all local asset references (src/href
# starting with a relative path, not http://), then times how long it
# takes to fetch every asset. Runs with the given concurrency (default 8)
# to simulate a browser loading a page.

set -euo pipefail

ZIM="${1:?usage: bench-serve.sh <file.zim> [concurrency]}"
CONCURRENCY="${2:-8}"

ZIM_BIN="${ZIM_BIN:-./target/release/zim}"
ZIM_ADDR="${ZIM_ADDR:-127.0.0.1:0}"

if [ ! -f "$ZIM" ]; then
    echo "error: $ZIM not found"
    exit 1
fi

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

echo "=== archive: $ZIM ($(du -h "$ZIM" | cut -f1)) ==="

# Extract the main page to find asset references
"$ZIM_BIN" serve "$ZIM" &
SERVER_PID=$!
sleep 1

# Find the listening port from the server output (we used port 0 for auto-assign)
# Since ZIM_ADDR defaults to 127.0.0.1:8080, we'll use that
ADDR="${ZIM_ADDR}"
if [ "$ADDR" = "127.0.0.1:0" ]; then
    echo "error: port 0 auto-assign not supported; set ZIM_ADDR=127.0.0.1:PORT"
    kill $SERVER_PID 2>/dev/null
    exit 1
fi

# Get main page, extract relative asset URLs
MAIN_HTML=$(curl -sf "http://$ADDR/")
ASSETS=$(echo "$MAIN_HTML" | grep -oE '(src|href)="[^"]*"' | sed 's/.*="//' | sed 's/"$//' | grep -v '^http[s]*://' | grep -v '^data:' | grep -v '^mailto:' | grep -v '^#' | sort -u)
COUNT=$(echo "$ASSETS" | wc -l | tr -d ' ')

if [ "$COUNT" -eq 0 ]; then
    echo "no local assets found"
    kill $SERVER_PID 2>/dev/null
    exit 0
fi

echo "found $COUNT local assets"

# Warm up
curl -sf -o /dev/null "http://$ADDR/" 2>/dev/null || true
for url in $ASSETS; do
    curl -sf -o /dev/null "http://$ADDR/$url" 2>/dev/null || true
done

echo ""
echo "=== per-asset median latency (5 runs, ms) ==="
{
    # Main page first
    TIMES=""
    for i in 1 2 3 4 5; do
        t=$(curl -s -o /dev/null -w '%{time_total}' "http://$ADDR/" 2>/dev/null || echo 0)
        TIMES="$TIMES $t"
    done
    MEDIAN=$(printf '%s\n' $TIMES | sort -n | sed -n '3p')
    SIZE=$(curl -s -o /dev/null -w '%{size_download}' "http://$ADDR/" 2>/dev/null || echo 0)
    printf "main_page %s %s /\n" "$MEDIAN" "$SIZE"

    for url in $ASSETS; do
        TIMES=""
        for i in 1 2 3 4 5; do
            t=$(curl -s -o /dev/null -w '%{time_total}' "http://$ADDR/$url" 2>/dev/null || echo 0)
            TIMES="$TIMES $t"
        done
        MEDIAN=$(printf '%s\n' $TIMES | sort -n | sed -n '3p')
        printf "asset %s %s /%s\n" "$MEDIAN" 0 "$url"
    done
} | sort -t' ' -k2 -n | while read type latency size url; do
    latency_ms=$(echo "$latency * 1000" | bc 2>/dev/null || echo 0)
    printf "  %6.1f ms  %7s B  %s\n" "$latency_ms" "$size" "$url"
done

echo ""
echo "=== full page load ($COUNT assets, $CONCURRENCY concurrent, 5 runs) ==="
for run in 1 2 3 4 5; do
    START=$(python3 -c 'import time; print(time.time())' 2>/dev/null || echo 0)
    echo "/ $(echo "$ASSETS")" | xargs -P"$CONCURRENCY" -I{} curl -sf -o /dev/null "http://$ADDR/{}" 2>/dev/null || true
    END=$(python3 -c 'import time; print(time.time())' 2>/dev/null || echo 0)
    if [ "$START" != 0 ] && [ "$END" != 0 ]; then
        ELAPSED=$(echo "($END - $START) * 1000" | bc 2>/dev/null || echo 0)
        echo "  run $run: ${ELAPSED}ms"
    fi
done

echo ""
echo "=== cluster stats ==="
# Use xxd to peek at the header
CLUSTER_COUNT=$(xxd -s 28 -l 4 -e "$ZIM" | awk '{print $2}')
ARTICLE_COUNT=$(xxd -s 24 -l 4 -e "$ZIM" | awk '{print $2}')
echo "articles: $ARTICLE_COUNT  clusters: $CLUSTER_COUNT"

kill $SERVER_PID 2>/dev/null || true
