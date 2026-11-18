#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DAQ_DIR="${SCRIPT_DIR}"
FLAKE_DIR="${REPO_ROOT}/nix_flake"

REMOTE_HOST="${DAQ_REMOTE_HOST:-root@45.76.8.71}"
REMOTE_DIR="${DAQ_REMOTE_DIR:-/opt/daqstuff}"
MAX_CACHE_MB="${DAQ_MAX_CACHE_MB:-96}"
MAX_CONCURRENT_DECODES="${DAQ_MAX_CONCURRENT_DECODES:-2}"

SYNC_DATA=false
for arg in "$@"; do
    case "$arg" in
        --sync-data)
            SYNC_DATA=true
            ;;
        -h|--help)
            echo "Usage: $(basename "$0") [--sync-data]"
            echo ""
            echo "Builds daqcloud and daqweb inside nix_flake, patches the binary for Debian,"
            echo "uploads to ${REMOTE_HOST}:${REMOTE_DIR}, and restarts daqcloud.service."
            echo ""
            echo "Options:"
            echo "  --sync-data   Also rsync local daq/cloud_store/ to the remote server"
            echo ""
            echo "Environment overrides:"
            echo "  DAQ_REMOTE_HOST   (default: root@45.76.8.71)"
            echo "  DAQ_REMOTE_DIR    (default: /opt/daqstuff)"
            exit 0
            ;;
        *)
            echo "Unknown option: $arg (use --help for usage)" >&2
            exit 1
            ;;
    esac
done

echo "==> [1/4] Building daqcloud & daqweb in nix_flake..."
nix develop "${FLAKE_DIR}" -c sh -c "cd '${DAQ_DIR}' && cargo build --release -p daqcloud && ./daqweb/build_web.sh"

TMP_BIN="$(mktemp /tmp/daqcloud-debian.XXXXXX)"
trap 'rm -f "${TMP_BIN}"' EXIT

echo "==> [2/4] Patching daqcloud ELF interpreter for Debian..."
cp "${DAQ_DIR}/target/release/daqcloud" "${TMP_BIN}"
nix shell nixpkgs#patchelf -c patchelf \
    --set-interpreter /lib64/ld-linux-x86-64.so.2 \
    --remove-rpath \
    "${TMP_BIN}"
chmod +x "${TMP_BIN}"

echo "==> [3/4] Uploading web bundle and daqcloud binary to ${REMOTE_HOST}..."
rsync -avz --delete "${DAQ_DIR}/daqweb/dist/" "${REMOTE_HOST}:${REMOTE_DIR}/dist/"
scp "${TMP_BIN}" "${REMOTE_HOST}:${REMOTE_DIR}/daqcloud.new"

if [[ "${SYNC_DATA}" == "true" ]]; then
    echo "==> Checkpointing local cloud_registry.sqlite and syncing cloud_store..."
    if [[ -f "${DAQ_DIR}/cloud_store/cloud_registry.sqlite" ]]; then
        nix develop "${FLAKE_DIR}" -c sqlite3 \
            "${DAQ_DIR}/cloud_store/cloud_registry.sqlite" \
            "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
    fi
    rsync -avz \
        --exclude='local_registry.sqlite*' \
        "${DAQ_DIR}/cloud_store/" \
        "${REMOTE_HOST}:${REMOTE_DIR}/cloud_store/"
fi

echo "==> [4/4] Swapping binary and restarting daqcloud.service..."
ssh "${REMOTE_HOST}" "
    set -e
    mv '${REMOTE_DIR}/daqcloud.new' '${REMOTE_DIR}/daqcloud'
    chmod +x '${REMOTE_DIR}/daqcloud'
    cat << EOF > /etc/systemd/system/daqcloud.service
[Unit]
Description=PER DAQ Cloud Server & Web Demo
After=network.target

[Service]
Type=simple
WorkingDirectory=${REMOTE_DIR}
Environment=RUST_LOG=info
Environment=MALLOC_ARENA_MAX=2
Environment=DAQCLOUD_WEB_DIST_DIR=${REMOTE_DIR}/dist
Environment=DAQCLOUD_MAX_CONCURRENT_DECODES=${MAX_CONCURRENT_DECODES}
ExecStart=${REMOTE_DIR}/daqcloud --bind 0.0.0.0:80 --store-dir ${REMOTE_DIR}/cloud_store --db-path ${REMOTE_DIR}/cloud_store/cloud_registry.sqlite --max-cache-mb ${MAX_CACHE_MB}
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl restart daqcloud.service
    sleep 1
    systemctl status daqcloud.service --no-pager | head -n 15
"

echo "==> Deploy complete! Live at http://${REMOTE_HOST#*@}/"
