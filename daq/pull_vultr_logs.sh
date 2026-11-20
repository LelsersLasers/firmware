#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LOGS_DIR="${REPO_ROOT}/logs"

REMOTE_HOST="${DAQ_REMOTE_HOST:-root@45.76.8.71}"

FOLLOW=false
LINES="500"

while [[ $# -gt 0 ]]; do
    case "$1" in
        -f|--follow)
            FOLLOW=true
            shift
            ;;
        -n|--lines)
            LINES="${2:?--lines requires a number}"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $(basename "$0") [-f|--follow] [-n|--lines N]"
            echo ""
            echo "Pulls daqcloud systemd journal logs from ${REMOTE_HOST} into logs/daqcloud_vultr.log"
            echo "and prints the most recent lines."
            echo ""
            echo "Options:"
            echo "  -f, --follow      Stream live logs from the server (Ctrl+C to exit)"
            echo "  -n, --lines N     Number of recent lines to print (default: 500, or 'all')"
            exit 0
            ;;
        *)
            echo "Unknown option: $1 (use --help for usage)" >&2
            exit 1
            ;;
    esac
done

mkdir -p "${LOGS_DIR}"
OUT_FILE="${LOGS_DIR}/daqcloud_vultr.log"

if [[ "${FOLLOW}" == "true" ]]; then
    echo "==> Streaming live daqcloud logs from ${REMOTE_HOST} (saving to ${OUT_FILE})..."
    ssh "${REMOTE_HOST}" "journalctl -u daqcloud.service -f --no-pager" | tee -a "${OUT_FILE}"
else
    echo "==> Pulling full daqcloud journal from ${REMOTE_HOST} -> ${OUT_FILE}..."
    ssh "${REMOTE_HOST}" "journalctl -u daqcloud.service --no-pager" > "${OUT_FILE}"
    TOTAL_LINES="$(wc -l < "${OUT_FILE}")"
    echo "==> Saved ${TOTAL_LINES} lines to ${OUT_FILE}"
    echo "--------------------------------------------------------------------------------"
    if [[ "${LINES}" == "all" ]]; then
        cat "${OUT_FILE}"
    else
        tail -n "${LINES}" "${OUT_FILE}"
    fi
fi
