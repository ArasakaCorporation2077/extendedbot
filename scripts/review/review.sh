#!/bin/bash
# 2-Stage PR Review Agent using Claude Code CLI
# Usage: ./scripts/review/review.sh [base_ref]
#   base_ref: git ref to diff against (default: HEAD~1)
#
# Examples:
#   ./scripts/review/review.sh              # review last commit
#   ./scripts/review/review.sh HEAD~3       # review last 3 commits
#   ./scripts/review/review.sh main         # review all changes since main

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BASE_REF="${1:-HEAD~1}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUTPUT_DIR="$PROJECT_ROOT/scripts/review/reports"
mkdir -p "$OUTPUT_DIR"

echo "=== Trading Bot PR Review Agent ==="
echo "Base ref: $BASE_REF"
echo ""

# Get the diff
DIFF=$(cd "$PROJECT_ROOT" && git diff "$BASE_REF" -- '*.rs' '*.toml')

if [ -z "$DIFF" ]; then
    echo "No Rust/TOML changes found since $BASE_REF"
    exit 0
fi

DIFF_STATS=$(cd "$PROJECT_ROOT" && git diff --stat "$BASE_REF" -- '*.rs' '*.toml')
echo "Changed files:"
echo "$DIFF_STATS"
echo ""

# --- Stage 1: Aggressive Issue Scanning ---
echo ">>> Stage 1: Scanning for issues..."
STAGE1_PROMPT=$(cat "$SCRIPT_DIR/stage1_scan.md")

STAGE1_RESULT=$(cd "$PROJECT_ROOT" && claude -p \
    "$STAGE1_PROMPT

--- DIFF START ---
$DIFF
--- DIFF END ---

Review this diff and find ALL potential issues." 2>/dev/null)

echo "$STAGE1_RESULT" > "$OUTPUT_DIR/stage1_${TIMESTAMP}.md"

# Count issues found
ISSUE_COUNT=$(echo "$STAGE1_RESULT" | grep -c "^ISSUE:" || true)
echo "Stage 1 found $ISSUE_COUNT potential issues."
echo ""

if [ "$ISSUE_COUNT" -eq 0 ]; then
    echo "No issues found. Review complete."
    exit 0
fi

# --- Stage 2: Verify with evidence ---
echo ">>> Stage 2: Verifying issues with evidence..."
STAGE2_PROMPT=$(cat "$SCRIPT_DIR/stage2_verify.md")

STAGE2_RESULT=$(cd "$PROJECT_ROOT" && claude -p \
    "$STAGE2_PROMPT

--- STAGE 1 ISSUES ---
$STAGE1_RESULT
--- END STAGE 1 ISSUES ---

Read the actual source files in this project and verify each issue above." 2>/dev/null)

echo "$STAGE2_RESULT" > "$OUTPUT_DIR/stage2_${TIMESTAMP}.md"

# --- Final Report ---
CONFIRMED=$(echo "$STAGE2_RESULT" | grep -c "VERDICT: CONFIRMED" || true)
FALSE_POS=$(echo "$STAGE2_RESULT" | grep -c "VERDICT: FALSE_POSITIVE" || true)
NEEDS_CTX=$(echo "$STAGE2_RESULT" | grep -c "VERDICT: NEEDS_CONTEXT" || true)

echo ""
echo "=== Review Complete ==="
echo "Stage 1 candidates: $ISSUE_COUNT"
echo "Confirmed:          $CONFIRMED"
echo "False positives:    $FALSE_POS"
echo "Needs context:      $NEEDS_CTX"
echo ""
echo "Full reports saved to:"
echo "  $OUTPUT_DIR/stage1_${TIMESTAMP}.md"
echo "  $OUTPUT_DIR/stage2_${TIMESTAMP}.md"

# Print only confirmed issues to stdout
if [ "$CONFIRMED" -gt 0 ]; then
    echo ""
    echo "=== CONFIRMED ISSUES ==="
    echo "$STAGE2_RESULT" | awk '/^ISSUE:/{issue=$0} /VERDICT: CONFIRMED/{found=1} found{print; if(/^$/){found=0}} /^ISSUE:/{if(found){print issue}}' || true
    echo ""
    echo "$STAGE2_RESULT"
fi
