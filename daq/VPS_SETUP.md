# PER DAQ Vultr VPS Configuration (`45.76.8.71`)

Concise reference for the production `daqcloud` + `daqweb` Vultr VPS configuration, security controls, and operational scripts.

---

## 1. Host & Storage Layout

| Property | Value |
| :--- | :--- |
| **Public IPv4** | `45.76.8.71` (`enp1s0`) |
| **Tailscale IPv4** | `100.75.133.29` (`tailscale0`) |
| **Tailscale MagicDNS** | `daqstuff0.tailef6d22.ts.net` (or short name `daqstuff0`) |
| **OS** | Debian 13 (Trixie) x86_64 |
| **Resources** | 1 vCPU, 512 MiB RAM + 1 GiB `/swapfile` (`vm.swappiness=10`) |
| **Service User** | `daqcloud` (unprivileged system user, `/usr/sbin/nologin`) |
| **Install Root** | `/opt/daqstuff` (owned by `daqcloud:daqcloud`) |
| **Binary** | `/opt/daqstuff/daqcloud` (ELF interpreter patched to `/lib64/ld-linux-x86-64.so.2`) |
| **Web Assets** | `/opt/daqstuff/dist/` (pre-built WASM + JS/HTML bundle served at `/`) |
| **Data Store** | `/opt/daqstuff/cloud_store/` (`partitions/`, `raw/`, `superdbcs/`, `cloud_registry.sqlite`) |
| **Secrets File** | `/opt/daqstuff/.env` (`root:daqcloud`, mode `0640`) |

---

## 2. Security Architecture

### A. SSH Hardening (`/etc/ssh/sshd_config.d/00-hardening.conf`)
Password and keyboard-interactive logins are disabled (`00-` prefix ensures first-match precedence ahead of `50-cloud-init.conf`); only Ed25519/RSA public keys in `/root/.ssh/authorized_keys` are accepted:
```text
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
```

### B. UFW Firewall Rules
- **`80/tcp` (ALLOW IN)**: Public HTTP for `daqweb` viewer and `/daqview/...` share links.
- **`22/tcp` (ALLOW IN)**: SSH key-only access (can optionally be restricted to `tailscale0` once Tailscale is linked).
- **`tailscale0` (ALLOW IN)**: All traffic over the Tailscale WireGuard interface.
- **Anti-Spoofing (`DENY IN on enp1s0`)**: Packets arriving on the public interface `enp1s0` claiming a Tailscale source IP (`100.64.0.0/10` or `fd7a:115c:a1e0::/48`) are dropped at the firewall.
- **`3000/tcp`**: Closed.

Check firewall status at any time:
```bash
ssh root@45.76.8.71 'ufw status numbered'
```

### C. Public Read-Only + Authenticated Writes
- **Public Read-Only (`GET` / `HEAD` / `POST /api/v1/export`)**: Anyone can open `http://45.76.8.71/`, load `/daqview/...` share links, view telemetry, and export CSV/Parquet without a password.
- **Protected Mutations (`PUT` / `POST` / `PATCH` / `DELETE` on partitions, annotations, derived signals, track days)**:
  A write request is permitted only if:
  1. The request originates from a Tailscale peer IP (`100.64.0.0/10` or `fd7a:115c:a1e0::/48`) while `DAQCLOUD_TRUST_TAILSCALE=true`, **or**
  2. The request includes the shared token from `/opt/daqstuff/.env` (`DAQCLOUD_WRITE_TOKEN`) via:
     - `Authorization: Bearer <token>` (used automatically by `daqweb`, `daqapp`, and `daqcli` when entered in **Settings ⚙ -> Write Password**)
     - `Authorization: Basic <base64(user:token)>`
     - `X-Daqcloud-Token: <token>`

### D. Per-IP Rate & Egress Bandwidth Limiting (Anti-Overcharge Protection)
To prevent bots or scrapers from burning monthly bandwidth or saturating the single vCPU, `daqcloud` enforces a rolling 60-second per-IP quota on public clients (Tailscale IPs, loopback, and valid write-token holders are exempt):
- **Egress Bandwidth Cap**: `150 MiB/min` per public IP (`DAQCLOUD_RATE_LIMIT_MB_PER_MIN=150`, ~10x a full cold track-day load).
- **General Request Cap**: `600 req/min` per public IP (`DAQCLOUD_RATE_LIMIT_REQS_PER_MIN=600`).
- **Heavy Export / Raw Parquet Cap**: `15 req/min` per public IP for `POST /api/v1/export` and raw `.parquet`/`.log` downloads.
- Exceeding any quota immediately returns `HTTP 429 Too Many Requests` (`Retry-After: 30`) before reading or decoding Parquet files.

---

## 3. Systemd Unit (`/etc/systemd/system/daqcloud.service`)

Managed automatically by [`daq/deploy_vultr.sh`](file:///home/millankumar/Documents/PER/daqstuff/monorepo_daqstuff_a/daq/deploy_vultr.sh):

```ini
[Unit]
Description=PER DAQ Cloud Server & Web Demo
After=network.target

[Service]
Type=simple
User=daqcloud
Group=daqcloud
WorkingDirectory=/opt/daqstuff
EnvironmentFile=-/opt/daqstuff/.env
Environment=RUST_LOG=info
Environment=MALLOC_ARENA_MAX=2
Environment=DAQCLOUD_WEB_DIST_DIR=/opt/daqstuff/dist
Environment=DAQCLOUD_MAX_CONCURRENT_DECODES=2
Environment=DAQCLOUD_TRUST_TAILSCALE=true
Environment=DAQCLOUD_RATE_LIMIT_MB_PER_MIN=150
Environment=DAQCLOUD_RATE_LIMIT_REQS_PER_MIN=600
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=full
PrivateTmp=true
ExecStart=/opt/daqstuff/daqcloud --bind 0.0.0.0:80 --store-dir /opt/daqstuff/cloud_store --db-path /opt/daqstuff/cloud_store/cloud_registry.sqlite --max-cache-mb 96
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

---

## 4. Common Operations

### Deploy Code & Web Bundle
```bash
./daq/deploy_vultr.sh
```
Also sync local `daq/cloud_store/` partitions to the VPS:
```bash
./daq/deploy_vultr.sh --sync-data
```

### View or Rotate the Write Password
View the current write password on the VPS:
```bash
ssh root@45.76.8.71 'cat /opt/daqstuff/.env'
```
Set a custom write password and restart `daqcloud`:
```bash
DAQ_WRITE_TOKEN='your-new-password' ./daq/deploy_vultr.sh
```

### Connect / Manage Tailscale on the VPS
Attach the VPS to your Tailscale tailnet (prints a login URL on first run):
```bash
ssh root@45.76.8.71 'tailscale up'
```
Check the VPS Tailscale `100.x.y.z` address:
```bash
ssh root@45.76.8.71 'tailscale ip -4'
```
*(Optional)* Lock SSH port `22` exclusively to Tailscale once your devices are on the tailnet:
```bash
ssh root@45.76.8.71 'ufw allow in on tailscale0 to any port 22 proto tcp && ufw delete allow 22/tcp'
```

### Pull Server Logs & Resource Snapshots
```bash
./daq/pull_vultr_logs.sh 1000
```
