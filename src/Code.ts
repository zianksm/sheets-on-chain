// ============================================================
// sheets-on-chain — server-side Apps Script (Code.ts)
// ============================================================

// --------------- Types ---------------

interface BatchCallRequest {
  fn: string;
  args: string[];
}

interface BatchCallResult {
  fn: string;
  args: string[];
  result: unknown;
}

interface CellFunction {
  fn: string;
  args: string[];
}

// --------------- Lifecycle ---------------

/** Called when the add-on is opened from the Extensions menu. */
function onOpen(): void {
  SpreadsheetApp.getActiveSpreadsheet()
    .addMenu("sheets-on-chain", [
      { name: "Open sidebar", functionName: "openSidebar" },
    ]);
}

/** Homepage trigger — required for add-on deployment. */
function onHomepage(): void {
  openSidebar();
}

function openSidebar(): void {
  const html = HtmlService.createHtmlOutputFromFile("Sidebar")
    .setTitle("sheets-on-chain")
    .setWidth(320);
  SpreadsheetApp.getUi().showSidebar(html);
}

// --------------- Config ---------------

function getConfig(): { backendUrl: string } {
  const props = PropertiesService.getUserProperties();
  return {
    backendUrl: props.getProperty("backendUrl") ?? "",
  };
}

function saveConfig(backendUrl: string): void {
  PropertiesService.getUserProperties().setProperty("backendUrl", backendUrl);
}

// --------------- Pinned Block ---------------

const PINNED_BLOCK_PROP = "pinnedBlock";

function getPinnedBlock(): number {
  const val = PropertiesService.getUserProperties().getProperty(PINNED_BLOCK_PROP);
  return val ? parseInt(val, 10) : 0;
}

function setPinnedBlock(block: number): void {
  PropertiesService.getUserProperties().setProperty(
    PINNED_BLOCK_PROP,
    block.toString()
  );
  // Also write to a named range so formula cells see a dependency and recalculate.
  const ss = SpreadsheetApp.getActiveSpreadsheet();
  let namedRange = ss.getRangeByName("PINNED_BLOCK");
  if (!namedRange) {
    // Create a cell in a hidden helper sheet to hold the value.
    let helperSheet = ss.getSheetByName("__soc_helper__");
    if (!helperSheet) {
      helperSheet = ss.insertSheet("__soc_helper__");
      helperSheet.hideSheet();
    }
    const cell = helperSheet.getRange("A1");
    ss.setNamedRange("PINNED_BLOCK", cell);
    namedRange = cell;
  }
  namedRange.setValue(block);
}

// --------------- Sheet Scanner ---------------

/**
 * Scans every cell in the active sheet for recognized custom-function calls
 * and returns a deduplicated list of {fn, args} pairs.
 *
 * Recognised pattern: =FN_NAME("arg1","arg2",...)
 * where FN_NAME is one of the registered functions.
 */
function getCellFunctions(): CellFunction[] {
  const KNOWN_FUNCTIONS = [
    "ETH_BALANCE",
    "ERC20_BALANCE",
    "ETH_BLOCK_NUMBER",
    "ETH_CALL",
  ];

  const sheet = SpreadsheetApp.getActiveSpreadsheet().getActiveSheet();
  const range = sheet.getDataRange();
  const formulas = range.getFormulas();

  // Pattern: =FNNAME( ... ) — captures the argument string inside the parens.
  const fnPattern = new RegExp(
    `=\\s*(${KNOWN_FUNCTIONS.join("|")})\\s*\\(([^)]*)\\)`,
    "gi"
  );

  const seen = new Set<string>();
  const results: CellFunction[] = [];

  for (const row of formulas) {
    for (const cell of row) {
      if (!cell) continue;
      let match: RegExpExecArray | null;
      while ((match = fnPattern.exec(cell)) !== null) {
        const fn = match[1].toUpperCase();
        // Parse the argument string into individual args.
        const rawArgs = match[2];
        const args = parseArgList(rawArgs);
        const key = `${fn}:${args.join(",")}`;
        if (!seen.has(key)) {
          seen.add(key);
          results.push({ fn, args });
        }
      }
      // Reset lastIndex since we're reusing the same regex object.
      fnPattern.lastIndex = 0;
    }
  }

  return results;
}

/**
 * Parses a comma-separated argument string, stripping surrounding quotes.
 * Handles simple string and numeric literals only.
 */
function parseArgList(raw: string): string[] {
  if (!raw.trim()) return [];
  return raw.split(",").map((a) =>
    a.trim().replace(/^["']|["']$/g, "")
  );
}

// --------------- Batch Result Writer ---------------

/**
 * Called from the sidebar after it receives results from the backend.
 * Writes each result into the user cache and bumps the pinned block
 * so that custom function cells recalculate.
 */
function writeBatchResults(
  block: number,
  results: BatchCallResult[]
): void {
  const cache = CacheService.getUserCache();
  const CACHE_TTL = 3600; // seconds — 1 hour

  const entries: { [key: string]: string } = {};
  for (const r of results) {
    const key = makeCacheKey(r.fn, r.args, block);
    entries[key] = JSON.stringify(r.result);
  }
  cache.putAll(entries, CACHE_TTL);

  setPinnedBlock(block);
}

function makeCacheKey(fn: string, args: string[], block: number): string {
  return `${fn}:${args.join(",")}:${block}`;
}

// --------------- Custom Functions ---------------

/**
 * Returns the ETH balance (in ETH) for the given address at the pinned block.
 * Cache-first: returns #N/A if the sidebar hasn't fetched yet.
 *
 * @param address Ethereum address (0x...)
 * @return Balance in ETH, or #N/A placeholder string
 * @customfunction
 */
function ETH_BALANCE(address: string): number | string {
  const block = getPinnedBlock();
  if (!block) return "#N/A";
  const cached = CacheService.getUserCache().get(
    makeCacheKey("ETH_BALANCE", [address], block)
  );
  if (cached !== null) return JSON.parse(cached) as number;
  return "#N/A";
}

/**
 * Returns the ERC-20 token balance for the given wallet at the pinned block.
 *
 * @param tokenAddress ERC-20 contract address (0x...)
 * @param walletAddress Wallet address (0x...)
 * @return Token balance (raw uint256 as string), or #N/A
 * @customfunction
 */
function ERC20_BALANCE(
  tokenAddress: string,
  walletAddress: string
): string {
  const block = getPinnedBlock();
  if (!block) return "#N/A";
  const cached = CacheService.getUserCache().get(
    makeCacheKey("ERC20_BALANCE", [tokenAddress, walletAddress], block)
  );
  if (cached !== null) return JSON.parse(cached) as string;
  return "#N/A";
}

/**
 * Returns the latest block number that the sidebar has synced.
 *
 * @return Block number, or #N/A
 * @customfunction
 */
function ETH_BLOCK_NUMBER(): number | string {
  const block = getPinnedBlock();
  return block > 0 ? block : "#N/A";
}

/**
 * Makes a read-only eth_call to a contract.
 *
 * @param contractAddress Contract address (0x...)
 * @param calldata ABI-encoded calldata hex string (0x...)
 * @return ABI-encoded return value hex string, or #N/A
 * @customfunction
 */
function ETH_CALL(contractAddress: string, calldata: string): string {
  const block = getPinnedBlock();
  if (!block) return "#N/A";
  const cached = CacheService.getUserCache().get(
    makeCacheKey("ETH_CALL", [contractAddress, calldata], block)
  );
  if (cached !== null) return JSON.parse(cached) as string;
  return "#N/A";
}
