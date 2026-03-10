#!/usr/bin/env bash
set -euo pipefail

# ── Colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; NC='\033[0m'

die()  { echo -e "${RED}❌ $*${NC}" >&2; exit 1; }
warn() { echo -e "${YELLOW}⚠  $*${NC}"; }
ok()   { echo -e "${GREEN}✔  $*${NC}"; }

# ── Load .env file if present ─────────────────────────────────────────────────
ENV_FILE="$(dirname "$0")/../.env"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
  ok "Loaded env from .env"
fi

# ── Dependency checks ─────────────────────────────────────────────────────────
command -v cargo-watch &>/dev/null \
  || die "cargo-watch not found. Install with: cargo install cargo-watch"

[[ -n "${ETH_RPC_URL:-}" ]] \
  || die "ETH_RPC_URL is not set.\n   Export it first:\n   export ETH_RPC_URL=wss://mainnet.infura.io/ws/v3/YOUR_KEY"

# ── Pick a tunnel tool (first one found wins) ─────────────────────────────────
if command -v ngrok &>/dev/null; then
  TUNNEL_CMD="ngrok http 3000"
  TUNNEL_NAME="NGROK"
elif command -v cloudflared &>/dev/null; then
  TUNNEL_CMD="cloudflared tunnel --url http://localhost:3000"
  TUNNEL_NAME="CF"
elif command -v bore &>/dev/null; then
  TUNNEL_CMD="bore local 3000 --to bore.pub"
  TUNNEL_NAME="BORE"
else
  die "No tunnel tool found. Install one of:
   ngrok:       brew install ngrok/ngrok/ngrok  (then: ngrok config add-authtoken TOKEN)
   cloudflared: brew install cloudflare/cloudflare/cloudflared
   bore:        cargo install bore-cli"
fi

ok "All dependencies found"
ok "Tunnel: ${TUNNEL_NAME} (${TUNNEL_CMD})"

# ── Read parentId from .clasp.json to build the sheet URL ─────────────────────
CLASP_JSON="$(cd "$(dirname "$0")/.." && pwd)/.clasp.json"
SHEET_ID=$(node -e "
  const fs = require('fs');
  const c = JSON.parse(fs.readFileSync('${CLASP_JSON}', 'utf8'));
  const id = Array.isArray(c.parentId) ? c.parentId[0] : c.parentId;
  if (id) process.stdout.write(id);
" 2>/dev/null || true)

# ── Open the Google Sheet once ────────────────────────────────────────────────
if [[ -n "$SHEET_ID" ]]; then
  SHEET_URL="https://docs.google.com/spreadsheets/d/${SHEET_ID}"
  ok "Opening sheet: ${SHEET_URL}"
  open "$SHEET_URL" 2>/dev/null || warn "Could not open browser automatically"
else
  warn "No parentId in .clasp.json — skipping sheet open"
  pnpm exec clasp open 2>/dev/null || true
fi

# ── Start all watchers via concurrently ───────────────────────────────────────
echo ""
echo "Starting dev watchers. Press Ctrl+C to stop all."
echo ""

pnpm exec concurrently \
  --names        "RUST   ,CLASP  ,${TUNNEL_NAME}" \
  --prefix-colors "blue.bold,green.bold,yellow.bold" \
  --kill-others \
  --kill-others-on-fail \
  "cd rust && ETH_RPC_URL=${ETH_RPC_URL} cargo watch -c -x run" \
  "pnpm exec clasp push --watch" \
  "${TUNNEL_CMD}"
