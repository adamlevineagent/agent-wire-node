# Vibesmithing Relay — Desktop App

> **Donate bandwidth, earn art.** Community-hosted CDN nodes that serve audio to listeners and get rewarded with art gifts through the drip system.

## What This Is

The Relay is a **Tauri v2 desktop app** (Rust backend + React frontend) that:

1. **Authenticates** with Supabase using a Vibesmithing account
2. **Downloads** audio tracks from Supabase Storage to local disk
3. **Serves** those tracks via a local HTTP server (port 8765)
4. **Reports** bandwidth usage back to Supabase for drip credit calculation

When a browser client detects an active relay, it fetches audio from the relay instead of the CDN — saving Vibesmithing money and earning the relay operator art gifts.

## Architecture

```
┌──────────────────────────────────────────────────┐
│  Browser (vibesmithing.com)                      │
│                                                  │
│  relay-client.ts                                 │
│    ├── Discovers active relays via Supabase REST │
│    ├── Queries relay /tracks for availability    │
│    └── Fetches audio from relay (3s timeout)     │
│         └── Falls back to CDN on failure         │
│                                                  │
│  relay-provider.tsx (React Context)              │
│    └── useRelay() hook for any component         │
│                                                  │
│  RelayIndicator.tsx                              │
│    └── ⚡ Relay badge in UnifiedAudioPlayer      │
└───────────────────┬──────────────────────────────┘
                    │ HTTP (port 8765)
                    ▼
┌──────────────────────────────────────────────────┐
│  Tauri Relay App (this directory)                │
│                                                  │
│  Rust Backend (src-tauri/src/)                   │
│    ├── main.rs    — System tray, IPC commands    │
│    ├── auth.rs    — Supabase login + heartbeat   │
│    ├── sync.rs    — Track download + cache mgmt  │
│    ├── server.rs  — Warp HTTP + Range requests   │
│    ├── bandwidth.rs — Stats + Supabase reporting │
│    └── lib.rs     — Shared state + config        │
│                                                  │
│  React Dashboard (src/)                          │
│    ├── App.tsx        — Auth gate                │
│    ├── LoginScreen    — Glassmorphic login       │
│    ├── Dashboard      — Real-time polling (2s)   │
│    ├── ImpactStats    — Money saved + stats grid │
│    ├── ActivityFeed   — Animated serve events    │
│    └── CacheStatus    — Track list + sync button │
└───────────────────┬──────────────────────────────┘
                    │ REST API
                    ▼
┌──────────────────────────────────────────────────┐
│  Supabase                                        │
│    ├── relay_nodes          (machine registry)   │
│    ├── relay_bandwidth_log  (daily bandwidth)    │
│    ├── relay_content        (cached tracks)      │
│    └── profiles.relay_annual_equivalent_cents     │
└──────────────────────────────────────────────────┘
```

## Prerequisites

- **Rust** (1.75+): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node.js** (20+): Required for the React frontend
- **Tauri CLI**: `cargo install tauri-cli --version "^2.0"`
- **System deps** (varies by OS):
  - **macOS**: Xcode Command Line Tools
  - **Windows**: Visual Studio C++ Build Tools, WebView2 (pre-installed on Win 11)
  - **Linux**: `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`

## Environment Variables

Create a `.env` file in the `relay-app/` directory (or set in your shell):

```bash
VITE_SUPABASE_URL=https://awszwexkdstzrintqdcn.supabase.co
VITE_SUPABASE_ANON_KEY=<your-anon-key>
```

The Rust backend reads these from the environment at runtime. The React frontend reads them via Vite's `import.meta.env`.

> **Note:** The default config in `lib.rs` has the Supabase URL and anon key hardcoded for PoC convenience. For production, use env vars.

## Quick Start

```bash
# From the project root:
cd relay-app

# Install frontend dependencies
npm install

# Run in development mode (opens Tauri window + Vite dev server)
cargo tauri dev
```

## Building for Production

```bash
# Build the distributable app
cargo tauri build
```

Output will be in `src-tauri/target/release/bundle/` — includes `.dmg` (macOS), `.msi` (Windows), or `.deb`/`.AppImage` (Linux).

## Configuration

Default config is in `src-tauri/src/lib.rs` → `RelayConfig`:

| Setting | Default | Description |
|---------|---------|-------------|
| `supabase_url` | Vibesmithing prod URL | Supabase project URL |
| `supabase_anon_key` | Vibesmithing prod key | Supabase anonymous key |
| `storage_cap_gb` | 5.0 | Max local disk usage for cached audio |
| `server_port` | 8765 | HTTP server port |
| `cache_dir` | `~/vibesmithing-relay/cache` | Where audio files are stored |
| `node_name` | `"My Relay Node"` | User-visible label |

## HTTP Server Endpoints

The relay serves audio on `http://localhost:8765`:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Status, version, tracks cached |
| `/tracks` | GET | List of available track IDs |
| `/audio/:track_id` | GET | Stream audio (supports `Range` header) |

All endpoints return CORS headers (`Access-Control-Allow-Origin: *`) and an `X-Served-By: vibesmithing-relay` header.

## Tauri IPC Commands

These commands are callable from the React frontend via `invoke()`:

| Command | Args | Returns | Description |
|---------|------|---------|-------------|
| `login` | `email`, `password` | `user_id` | Authenticates and registers relay node |
| `get_auth_state` | — | `AuthState` | Current auth info |
| `sync_content` | — | `SyncState` | Downloads tracks to cache |
| `get_sync_state` | — | `SyncState` | Current cache state |
| `get_bandwidth_stats` | — | `DashboardStats` | Formatted bandwidth stats |
| `get_activity_feed` | — | `ServeEvent[]` | Recent serve activity |
| `get_relay_config` | — | `RelayConfig` | Current config |

## Database Tables

These tables must exist in Supabase (migration: `20260209_relay_tables.sql`):

### `relay_nodes`
| Column | Type | Purpose |
|--------|------|---------|
| `id` | UUID PK | Node identity |
| `user_id` | UUID FK → profiles | Owner |
| `node_name` | TEXT | Human label |
| `storage_cap_gb` | FLOAT | Allocated disk |
| `status` | TEXT | `active`, `paused`, `offline` |
| `relay_endpoint` | TEXT | HTTP endpoint URL |
| `last_seen_at` | TIMESTAMPTZ | Heartbeat timestamp |
| `total_bandwidth_served_bytes` | BIGINT | Lifetime total |

### `relay_bandwidth_log`
| Column | Type | Purpose |
|--------|------|---------|
| `node_id` | UUID FK → relay_nodes | Which node |
| `date` | DATE | Day bucket |
| `bytes_served` | BIGINT | Bytes served that day |
| `tracks_served` | INTEGER | Tracks served |
| `unique_peers_served` | INTEGER | Unique listeners |

### `relay_content`
| Column | Type | Purpose |
|--------|------|---------|
| `node_id` | UUID FK → relay_nodes | Which node |
| `track_id` | UUID FK → tracks | Which track |
| `file_hash` | TEXT | SHA-256 integrity check |
| `file_size_bytes` | BIGINT | Size on disk |

## Network Setup for External Access

### PoC (Local Network / Port Forward)
The relay serves on port 8765. For testing on the same network, access via the machine's LAN IP.

For external access, port-forward 8765 on your router.

### Production (Recommended)
Use **Cloudflare Tunnel** for secure, NAT-traversing HTTPS without port forwarding:

```bash
# Install cloudflared
# macOS: brew install cloudflared
# Windows: winget install Cloudflare.cloudflared

# Create tunnel (one-time)
cloudflared tunnel login
cloudflared tunnel create vibesmithing-relay
cloudflared tunnel route dns vibesmithing-relay relay-yourname.vibesmithing.com

# Run tunnel (alongside the relay app)
cloudflared tunnel run --url http://localhost:8765 vibesmithing-relay
```

Then set the `relay_endpoint` in Supabase to `https://relay-yourname.vibesmithing.com`.

## How Drip Credits Work

1. **Daily cron** (`/api/cron/relay-credits`) sums each operator's 30-day bandwidth
2. **Annual equivalent** calculated at $0.09/GB (Supabase bandwidth cost)
3. Written to `profiles.relay_annual_equivalent_cents`
4. The **drip system** treats this as patronage — relay operators receive art gifts proportional to their bandwidth contribution

**Example:** Serving 10 GB/month → $0.90/mo → $10.80/yr equivalent → same drip rate as a ~$1/mo patron.

## Browser Integration (in vibesmithing-web)

The browser-side code lives in the main web app, not this directory:

| File | Location | Purpose |
|------|----------|---------|
| `relay-client.ts` | `src/lib/relay/` | Peer discovery + CDN fallback |
| `relay-provider.tsx` | `src/lib/relay/` | React context + `useRelay()` hook |
| `RelayIndicator.tsx` | `src/components/relay/` | ⚡ badge in player |
| `ServiceWorkerRegistration.tsx` | `src/components/relay/` | Phase 0 SW cache registration |
| `sw.js` | `public/` | Service Worker (CacheFirst for audio/images) |

## Rollback

If the relay feature is abandoned:

1. Run `supabase/migrations/20260209_relay_tables_rollback.sql` against the database
2. Remove `relay-app/` directory
3. Remove `src/lib/relay/`, `src/components/relay/`, `src/app/api/cron/relay-credits/`
4. Revert layout.tsx and UnifiedAudioPlayer.tsx changes
5. The Service Worker cache (Phase 0) can stay — it's independently useful

## File Tree

```
relay-app/
├── index.html              # Vite entry point
├── package.json            # React + Tauri frontend deps
├── vite.config.ts          # Vite config for Tauri
├── tsconfig.json           # TypeScript config
├── README.md               # ← You are here
├── src/                    # React dashboard frontend
│   ├── main.tsx
│   ├── App.tsx
│   ├── components/
│   │   ├── LoginScreen.tsx
│   │   ├── Dashboard.tsx
│   │   ├── ImpactStats.tsx
│   │   ├── ActivityFeed.tsx
│   │   └── CacheStatus.tsx
│   └── styles/
│       └── dashboard.css
└── src-tauri/              # Rust backend
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── build.rs
    └── src/
        ├── lib.rs          # Shared state + config
        ├── main.rs         # Entry point, system tray, IPC
        ├── auth.rs         # Supabase auth
        ├── sync.rs         # Content sync engine
        ├── server.rs       # HTTP server (warp)
        └── bandwidth.rs    # Stats + reporting
```
