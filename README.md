# sheets-on-chain

A Google Sheets add-on that exposes on-chain Ethereum data as native spreadsheet functions.

```
=ETH_BALANCE("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
=ERC20_BALANCE("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
=ETH_BLOCK_NUMBER()
=ETH_CALL("0x...", "0x...")
```

A sidebar controls whether cells track the **latest block** (live via WebSocket) or a **pinned historical block** (slider). All blockchain data flows through a self-hosted Rust backend — no API keys ever touch the spreadsheet.

---

## How it works

```
Google Sheets cell          sidebar JS              Rust backend        Ethereum node
=ETH_BALANCE("0x...")  ←─── CacheService ←── writeBatchResults ←── POST /  ←──  RPC
                             (cache miss → #N/A)                  (batch)
                        ─── getCellFunctions ──►
                                                 WebSocket newHeads ◄── WS subscription
```

1. **Custom functions** are cache-first — they read `CacheService` and return `#N/A` until the sidebar fetches data.
2. The **sidebar** scans the sheet for formula calls, sends a JSON-RPC batch to the backend, then writes results back into the cache and bumps the pinned block (triggering recalculation).
3. In **live mode** the sidebar holds a WebSocket subscription to the backend; every new block header triggers a fresh batch fetch automatically.

---

## Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| Node.js | ≥ 18 | https://nodejs.org |
| Rust + Cargo | stable | `curl https://sh.rustup.rs -sSf \| sh` |
| pnpm | ≥ 9 | `npm i -g pnpm` or https://pnpm.io/installation |
| clasp | ≥ 2.4 | installed via `pnpm install` below |

---

## Setup

### 1. Install JS dependencies

```sh
pnpm install
```

This installs `@types/google-apps-script` (for TypeScript intellisense) and `clasp`.

### 2. Authenticate with Google

```sh
pnpm exec clasp login
```

### 3. Create the Apps Script project

```sh
pnpm exec clasp create --type sheets --title "sheets-on-chain"
```

This overwrites `.clasp.json` with your real `scriptId`.

### 4. Push the add-on

```sh
pnpm exec clasp push
```

Clasp compiles the TypeScript in `src/` and uploads it. Re-run after any code change.

To watch for changes and push automatically:

```sh
pnpm exec clasp push --watch
```

### 5. Open a test sheet

```sh
pnpm exec clasp open
```

In the sheet go to **Extensions → sheets-on-chain → Open sidebar**.

---

## Running the backend

The Rust backend proxies JSON-RPC calls and forwards `newHeads` WebSocket subscriptions to the sidebar.

### Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `ETH_RPC_URL` | yes | WebSocket URL of your Ethereum node, e.g. `wss://mainnet.infura.io/ws/v3/KEY` |
| `LISTEN_ADDR` | no | Address to bind (default `127.0.0.1:3000`) |

### Run (development)

```sh
cd rust
ETH_RPC_URL=wss://mainnet.infura.io/ws/v3/YOUR_KEY cargo run
```

With a custom listen address:

```sh
ETH_RPC_URL=wss://... LISTEN_ADDR=0.0.0.0:8080 cargo run
```

Or with explicit flags:

```sh
cargo run -- --rpc-url wss://... --listen 0.0.0.0:8080
```

### Build (release)

```sh
cd rust
cargo build --release
# binary at rust/target/release/backend
```

### CORS

The backend does not enforce CORS — the sidebar runs inside Google's iframe and does not send `Origin` headers that browsers would block. If you expose the backend publicly, add your own reverse proxy with appropriate CORS / auth headers.

---

## Supported functions

| Function | Description |
|----------|-------------|
| `=ETH_BALANCE(address)` | ETH balance in wei (hex) at the pinned block |
| `=ERC20_BALANCE(token, wallet)` | ERC-20 `balanceOf` result (raw hex uint256) |
| `=ETH_BLOCK_NUMBER()` | The block number currently pinned by the sidebar |
| `=ETH_CALL(contract, calldata)` | Raw `eth_call` — returns ABI-encoded hex result |

All functions return `#N/A` until the sidebar has fetched data for the current block. Once fetched, results are cached for 1 hour and survive sidebar close.

---

## Project structure

```
sheets-on-chain/
  .clasp.json          # Apps Script project link (set scriptId after clasp create)
  appsscript.json      # Add-on manifest (scopes, homepage trigger)
  package.json         # Dev deps: clasp, TypeScript, GAS type stubs
  tsconfig.json
  src/
    Code.ts            # Server-side: custom functions, cache, sheet scanner
    Sidebar.html       # Client-side: block slider, live mode, batch fetch
  rust/
    Cargo.toml
    src/main.rs        # JSON-RPC batch proxy + WebSocket newHeads forwarder
```

---

## Development tips

- **Type checking only** (no push): `pnpm exec tsc --noEmit`
- **View server logs**: `pnpm exec clasp logs --watch`
- **Increase log verbosity** (backend): `RUST_LOG=debug cargo run -- ...`
- **Local HTTPS** (required if sidebar uses `https://`): use a tunnel like `ngrok http 3000`
