# quicsync

**English** | [한국어](README.ko.md)

A single Rust binary that runs rsync's delta-sync over a QUIC (UDP) tunnel, so transfers stay fast on long-distance links.

## Why quicsync?

Over a long fat network (RTT ≥ 100 ms), rsync over SSH leaves most of the link idle: the TCP window caps it at 10–20% of the available bandwidth. quicsync fixes that without touching rsync. It intercepts rsync's TCP traffic locally and relays it through a QUIC tunnel.

- rsync's delta-sync algorithm runs unchanged
- A bounded TCP↔QUIC relay with QUIC flow control relieves long-distance bottlenecks
- QUIC BBR congestion control uses the link's full bandwidth
- Your existing SSH stays the authentication channel

## How it works

```
quicsync user@remote:/path /local/path
    │
    ├─ 1. Launch the remote quicsync server over SSH (receive port + token)
    ├─ 2. Establish a QUIC tunnel (quinn, TLS 1.3, BBR)
    ├─ 3. Bind a local TCP proxy port
    ├─ 4. Spawn the rsync child process (destination → local proxy)
    │
    │  rsync ←TCP→ TCP_Proxy ←Relay→ QUIC_Tunnel ←QUIC→ Remote_Server ←TCP→ rsync(server)
    │
    └─ 5. Transfer completes → clean up resources
```

One binary does both sides: CLI mode locally, `--server` mode remotely. No daemon, no port forwarding.

## Install

### Build from source

```bash
# Requires Rust 1.85+ (edition 2024)
git clone https://github.com/JINWOO-J/quicsync.git
cd quicsync
cargo build --release

# Copy the binary into your PATH
cp target/release/quicsync /usr/local/bin/
```

Both hosts need the `quicsync` binary. If the remote doesn't have it, quicsync can deploy the right one for you (see [Remote install](#remote-install)).

### Self-update

```bash
quicsync update --check
quicsync update
```

`update` pulls the matching release asset from GitHub Releases, checks its SHA-256, and swaps the running binary in place. See [Self-update](#self-update) for the details.

## Usage

Same shape as rsync:

```bash
# Push: local → remote
quicsync /local/dir user@remote:/remote/dir

# Pull: remote → local
quicsync user@remote:/remote/dir /local/dir

# Pass rsync options through
quicsync -avz --delete --exclude='*.tmp' /src user@server:/dst

# Multiple source paths (glob)
quicsync ./* user@host:/dst

# Set the QUIC window (default 64 MB; raise it for high RTT)
quicsync --window 128 /src user@host:/dst

# Live web monitor during the transfer
quicsync --web /src user@host:/dst

# Preflight diagnostics
quicsync doctor user@host

# Install quicsync on the remote if missing (auto-picks the right OS/arch)
quicsync install-remote user@host

# Install on the remote and retry once if it's missing at transfer time
quicsync --install-remote /src user@host:/dst

# Explicitly fall back to rsync-over-SSH if QUIC init fails
quicsync --fallback=rsync /src user@host:/dst
```

quicsync adds `--stats` to rsync for you, so rsync prints a summary when it finishes. Want quicsync's own numbers too? Pass `--stats`. The direction and paths show at the start, the elapsed time at the end.

### Supported path forms

| Form | Meaning |
|------|---------|
| `user@host:path` | Remote path with explicit user |
| `host:path` | Remote path as the current user |
| `/absolute/path` | Local absolute path |
| `./relative/path` | Local relative path |

Push mode takes multiple source paths, globs included. Pull mode takes exactly one remote source.

### quicsync options

| Option | Description | Default |
|--------|-------------|---------|
| `--window MB` | QUIC flow-control window size (MB) | 64 |
| `--web` | Serve a live monitoring dashboard on localhost during the transfer | false |
| `--no-progress` | Disable the TTY progress line | false |
| `--stats` | Print quicsync's own transfer stats | false |
| `--stats-format text\|json` | quicsync stats output format | text |
| `--fallback none\|rsync` | Retry over rsync-over-SSH if QUIC session init fails | none |
| `--install-remote` | Install `quicsync` on the remote if missing, then retry once | false |

Anything else is passed through as an rsync option or a path. Experimental options that aren't wired into the real transfer path yet are rejected outright rather than silently accepted.

### Web monitor

Pass `--web` to watch the transfer in your browser. quicsync runs a localhost-only HTTP server for the length of the transfer, with no extra dependencies. It binds `127.0.0.1` on an ephemeral port, prints the URL to stderr, and opens the browser for you.

```bash
quicsync --web /src user@host:/dst
# quicsync: web monitor → http://127.0.0.1:51550
```

The dashboard polls `/api/metrics` every 500 ms and shows:

- Throughput, bytes transferred, elapsed time, transport mode (QUIC/TCP)
- File progress: the current file, completed/total files, and a progress bar

In **push** mode quicsync walks the local source to get the total file count, which drives the percentage bar; in **pull** mode it shows only the completed count. To see the dashboard without running a transfer:

```bash
cargo run --example web_dashboard
```

### Diagnostics

`doctor` checks your local and remote dependencies, and whether a QUIC tunnel can come up at all, before you start a transfer.

```bash
quicsync doctor user@host
quicsync doctor --json user@host
```

It covers local `rsync`, local `quicsync`, SSH connectivity, remote `quicsync`, remote `rsync`, and the QUIC handshake. A failed check carries a cause-specific `hint` where one applies, and `--json` keeps the same hints.

### Remote install

When the remote doesn't have `quicsync`, quicsync deploys it. It reads the remote OS and architecture from `uname` and picks the matching binary:

- **Same platform as local** → it streams your current binary over SSH (nothing to download)
- **Different platform** → it downloads the matching release asset (`quicsync_<os>_<arch>.tar.gz`), verifies the SHA-256, and installs over SSH

```bash
quicsync install-remote user@host
quicsync install-remote --dir /usr/local/bin user@host
quicsync --install-remote /src user@host:/dst
```

Installs land in `$HOME/.local/bin/quicsync` by default. `--install-remote` kicks in only when a transfer finds the remote `quicsync` missing, then retries once. A cross-architecture install fetches the asset for the **same version** as your local binary, so that release has to exist on GitHub Releases.

### Self-update

```bash
quicsync update --check
quicsync update
quicsync update --to v0.4.0
```

What `update` does depends on where the running binary lives. From a Homebrew path it hands off to `brew upgrade quicsync`. From a Cargo-install path it prints the `cargo install --git https://github.com/jinwoo-j/quicsync quicsync --force` command. Anywhere else (a manual install), it downloads `quicsync_<os>_<arch>.tar.gz` and `checksums.txt` from GitHub Releases, verifies the SHA-256, and replaces the current binary atomically. `--check` exits 1 when a newer version is out.

## Benchmarks

Symmetric latency injected between Docker containers with pumba (tc netem). Single 128 MB file, averaged over 3 rounds.

| RTT | rsync+ssh | quicsync | Speedup |
|-----|-----------|----------|---------|
| 0ms | 0.96s (156 MB/s) | 0.99s (147 MB/s) | 0.97x |
| 50ms | 11.14s (12.4 MB/s) | 3.03s (43.0 MB/s) | **3.67x** |
| 100ms | 22.05s (6.1 MB/s) | 5.09s (25.2 MB/s) | **4.33x** |
| 200ms | 63.24s (2.2 MB/s) | 9.31s (13.7 MB/s) | **6.79x** |
| 500ms | 106.14s (1.3 MB/s) | 20.51s (6.2 MB/s) | **5.18x** |

On a LAN (RTT 0 ms), QUIC's userspace overhead puts it on par with rsync+ssh. The gap opens as RTT climbs: TCP is capped by `throughput ≈ window_size / RTT`, while QUIC (BBR) with a 64 MB window keeps the pipe full.

### Running the benchmarks

```bash
# Docker-based latency sweep (uses pumba)
docker compose -f bench/docker/compose.yml up -d --build
bash bench/docker/setup_ssh.sh
bash bench/docker/run_latency_sweep.sh 3
docker compose -f bench/docker/compose.yml down

# Real-server benchmark (requires gtime)
brew install gnu-time
./bench/run.sh user@host:/remote/path 3
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `QUICSYNC_BUFFER_SIZE` | `268435456` (256MB) | Internal buffer-layer allocation (bytes). The relay leans on bounded-channel backpressure, so reach for `--window` first when tuning. |
| `QUICSYNC_WINDOW` | `64` | QUIC window size (MB). Raise it on high-RTT links for more throughput. |
| `QUICSYNC_DEFAULT_ARGS` | unset | Default rsync options prepended to every transfer (e.g. `-a`); user-supplied options come after and take precedence. quicsync warns when no recursive flag (`-a`/`-r`/`-d`) is present, since rsync silently skips directories otherwise. |
| `RUST_LOG` | unset | Log filter, e.g. `RUST_LOG=debug`, `RUST_LOG=quicsync=trace`. |

```bash
# 128 MB QUIC window (very high RTT)
quicsync --window 128 /src user@host:/dst

# Or via env var (also applied on the server side)
QUICSYNC_WINDOW=128 quicsync /src user@host:/dst

# 512 MB buffer
QUICSYNC_BUFFER_SIZE=536870912 quicsync /src user@host:/dst

# Always pass -a (archive) so directories sync without retyping it
QUICSYNC_DEFAULT_ARGS=-a quicsync host:~/data ./data

# Debug logs
RUST_LOG=debug quicsync /src user@host:/dst
```

## Build & test

```bash
# Build
cargo build

# Full test suite (unit, integration, property-based)
cargo test

# Release build
cargo build --release

# Local E2E smoke test
scripts/e2e-local.sh localhost
```

The suite includes `proptest` property tests for CLI parsing, the ring buffer, the handshake protocol, data integrity, auth tokens, rsync command construction, and exit-code propagation. The E2E smoke harness runs only when `ssh localhost` works without a password and `quicsync` is on the remote PATH; otherwise it skips with exit 77.

## Project structure

```
src/
├── main.rs        # Entry point (CLI / --server dispatch)
├── lib.rs         # Module declarations
├── cli.rs         # CLI argument parsing (clap)
├── ssh.rs         # SSH remote-server launch and handshake
├── quic.rs        # QUIC tunnel (quinn, BBR, TLS 1.3)
├── tcp_proxy.rs   # Local TCP proxy
├── buffer.rs      # Ring buffer and async relay
├── server.rs      # Remote QUIC server
├── rsync.rs       # rsync child-process management
├── session.rs     # Session orchestrator and signal handling
├── error.rs       # Error type hierarchy
├── types.rs       # Core data models
├── metrics.rs     # Transfer metrics
├── progress.rs    # TTY progress line
├── stats.rs       # quicsync stats output
├── web.rs         # Live monitoring web server (--web)
├── remote_install.rs # Remote install (same-arch copy / cross-arch download)
├── update.rs      # Self-update + shared release downloader
├── integrity.rs   # Blake3 integrity utilities (not on transfer path)
├── multi_stream.rs # Multi-stream experimental infra (not on transfer path)
└── telemetry.rs   # OpenTelemetry experimental infra (feature-gated)
examples/
└── web_dashboard.rs # --web dashboard preview with dummy metrics
```

## Key dependencies

| Crate | Purpose |
|-------|---------|
| `quinn` | QUIC protocol implementation |
| `tokio` | Async runtime |
| `rustls` | TLS 1.3 |
| `clap` | CLI argument parsing |
| `rcgen` | Self-signed certificate generation |
| `ring` | Cryptographic primitives |

## Limitations

- Both hosts need `quicsync`. A missing remote can be deployed with `install-remote` / `--install-remote`, across OS/architecture via release assets.
- Targets Linux (x86_64/aarch64) and macOS (x86_64/aarch64).
- On UDP-blocked or QUIC-init-failing networks, pass `--fallback=rsync` to retry over TCP/SSH rsync.
- rsync daemon mode (`rsync://`) is not supported.
- Multi-stream file transfer, OpenTelemetry export, and per-chunk Blake3 verification aren't wired into the real transfer path yet.

## License

MIT
