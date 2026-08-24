//! Captures the operator-console documentation screenshots.
//!
//! The live view is data-driven by design, so the shots are produced by
//! driving the real built app with scripted worker frames over a stubbed
//! WebSocket — the same validated path a real session takes, minus the
//! network. No module mocks, no photoshopped numbers: every pixel comes out
//! of `applyLiveFrame`.
//!
//! Usage:
//!
//! ```bash
//! npm --prefix dashboard run build   # dist/ must exist first
//! npm --prefix dashboard run shots   # writes ../docs/screenshots/*.png
//! ```
//!
//! States:
//! - `engaged`  — runner reported posture: kill switch engaged, order entry closed.
//! - `disarmed` — operator disarmed a running session; controls live, open
//!                positions and fill reconciliation visible.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { mkdirSync } from "node:fs";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const DIST = join(ROOT, "dist");
const OUT_DIR = fileURLToPath(new URL("../../docs/screenshots", import.meta.url));
const PORT = 4173;

const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
};

function serveDist() {
  const server = createServer(async (request, response) => {
    try {
      const path = normalize(decodeURIComponent(new URL(request.url, "http://x").pathname));
      const file = join(DIST, path === "/" ? "index.html" : path);
      const body = await readFile(file);
      response.writeHead(200, {
        "content-type": MIME[extname(file)] ?? "application/octet-stream",
      });
      response.end(body);
    } catch {
      // SPA fallback: client-side routes get the shell.
      const body = await readFile(join(DIST, "index.html"));
      response.writeHead(200, { "content-type": "text/html" });
      response.end(body);
    }
  });
  return new Promise((resolve) => server.listen(PORT, "127.0.0.1", () => resolve(server)));
}

// ---------------------------------------------------------------------------
// Scripted worker frames. Integer cents/bps throughout, exactly as the wire
// schema demands; the reducer validates and adopts them like any session.
// ---------------------------------------------------------------------------

const SESSION = {
  runId: "1787530482650-linux-x86_64-2bd8baa-live",
  startedAtEpochMs: 1_787_530_482_650,
  initialBankrollCents: 10_000_000,
  ticksPerWindow: 200,
  windowMs: 1_000,
};

const TOTALS = {
  trades: 41,
  stakedCents: 3_980_000,
  theoreticalProfitCents: 121_300,
  realizedProfitCents: 104_620,
  expectedProfitCents: 118_900,
  feesPaidCents: 96_140,
  roiTheoreticalBps: 304,
  roiRealizedBps: 262,
};

const FUNNEL = {
  attempted: 58,
  capitalShort: 16,
  clean: 34,
  proportional: 3,
  phantom: 4,
  brokenLeg: 0,
};

const CAPITAL = { lockedCents: 249_000, availableCents: 9_124_620 };

function record(seq, overrides = {}) {
  return {
    seq,
    detectionTimestampNs: 800_188_630 + seq * 1_000,
    latencyNs: 1_658_669,
    marketLabel: "Boston Celtics @ Los Angeles Lakers · moneyline",
    edgeBps: 458,
    overroundPpm: 956_100,
    requestedStakeCents: 100_000,
    expectedProfitCents: 4_583,
    worstCaseProfitCents: 4_583,
    realizedProfitCents: 4_583,
    slippageCents: 120,
    feesPaidCents: 3_661,
    fillRatioBps: 10_000,
    classification: "clean",
    chased: false,
    legs: [],
    ...overrides,
  };
}

/** Runner-reported risk envelopes. The engaged shot is exactly what the
 * paper runner streams at session open; the disarmed shot shows the full
 * envelope once a runner enforces every cap. */
const RISK_ENGAGED = {
  executionMode: "paper",
  killSwitch: true,
  maxStakePerLegCents: null,
  maxDailyLossCents: null,
  dailyLossUsedCents: null,
  maxOpenTrades: null,
  openTrades: null,
  minEdgeBps: null,
};

const RISK_DISARMED = {
  executionMode: "paper",
  killSwitch: false,
  maxStakePerLegCents: 5_000,
  maxDailyLossCents: 50_000,
  dailyLossUsedCents: 12_345,
  maxOpenTrades: 1,
  openTrades: 1,
  minEdgeBps: 50,
};

const FILLS = [
  {
    clientOrderId: "9f31c7a2b64d81e0",
    venueOrderId: "kalshi-10233984",
    tradeSeq: 40,
    filledStakeCents: 99_500,
    realizedProfitCents: null,
    settlementStatus: "open",
    reconciledAtEpochMs: 1_787_530_498_210,
  },
  {
    clientOrderId: "5c88d0f19e23aa47",
    venueOrderId: "poly-0x51ab",
    tradeSeq: 39,
    filledStakeCents: 100_000,
    realizedProfitCents: 4_102,
    settlementStatus: "settled",
    reconciledAtEpochMs: 1_787_530_496_880,
  },
];

const ITEMS = [
  record(38, { classification: "proportional", realizedProfitCents: 2_911 }),
  record(39),
  record(40, {
    executionMode: "paper",
    venueOrderIds: ["kalshi-10233984", "poly-0x51ab"],
    filledStakeCents: 99_500,
    settlementStatus: "open",
    realizedProfitCents: null,
  }),
];

const FRAMES_BASE = [
  { t: "hello", serverTimeEpochMs: Date.now() },
  {
    t: "snapshot",
    status: "live",
    session: SESSION,
    risk: RISK_ENGAGED,
    fills: [],
    totals: TOTALS,
    funnel: FUNNEL,
    capital: CAPITAL,
    windowsCompleted: 12,
    seqCursor: 38,
    items: ITEMS,
  },
  { t: "fills", items: FILLS },
  {
    t: "totals",
    status: "live",
    risk: RISK_ENGAGED,
    totals: TOTALS,
    funnel: FUNNEL,
    capital: CAPITAL,
    windowsCompleted: 12,
    seqCursor: 38,
  },
  {
    // One window further on: the ROI sparkline samples on change, and an
    // identical repeat push is deliberately not a sample.
    t: "totals",
    status: "live",
    risk: RISK_ENGAGED,
    totals: { ...TOTALS, trades: 42, realizedProfitCents: 108_940, roiRealizedBps: 273 },
    funnel: FUNNEL,
    capital: CAPITAL,
    windowsCompleted: 13,
    seqCursor: 39,
  },
];

const STATES = {
  engaged: FRAMES_BASE,
  disarmed: FRAMES_BASE.map((frame) =>
    frame.t === "snapshot" || frame.t === "totals"
      ? { ...frame, risk: RISK_DISARMED }
      : frame,
  ),
};

// ---------------------------------------------------------------------------
// Browser-side WebSocket stub, injected before the app boots.
// ---------------------------------------------------------------------------

function webSocketStub(framesJson) {
  class StubSocket {
    static OPEN = 1;
    constructor() {
      this.readyState = 0;
      this.sent = [];
      this.handlers = new Map();
      const timers = [];
      this.close = () => {
        this.readyState = 3;
        this.emit("close", new Event("close"));
        timers.forEach(clearTimeout);
      };
      this.send = (data) => this.sent.push(data);
      setTimeout(() => {
        this.readyState = 1;
        this.emit("open", new Event("open"));
        framesJson.forEach((frame, index) => {
          timers.push(
            setTimeout(() => {
              this.emit("message", new MessageEvent("message", { data: frame }));
            }, 60 * (index + 1)),
          );
        });
      }, 30);
    }
    addEventListener(type, handler) {
      this.handlers.set(type, [...(this.handlers.get(type) ?? []), handler]);
    }
    emit(type, event) {
      for (const handler of this.handlers.get(type) ?? []) handler(event);
    }
  }
  return StubSocket;
}

async function capture(browser, stateName, outFile, selector) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    deviceScaleFactor: 2,
  });

  // `addInitScript` cannot close over this module, so serialize the stub
  // itself alongside the frames and rebuild it inside the page.
  const page = await context.newPage();
  await page.addInitScript(
    ({ stubSource, framesJson }) => {
      const makeStub = new Function(`return (${stubSource})`)();
      window.WebSocket = makeStub(framesJson);
    },
    {
      stubSource: webSocketStub.toString(),
      framesJson: STATES[stateName].map((frame) => JSON.stringify(frame)),
    },
  );

  await page.goto(`http://127.0.0.1:${PORT}/#live`);
  await page.waitForSelector(".operator-console");
  await page.waitForFunction(
    () => document.querySelector('[data-testid="kill-switch-state"]') !== null,
  );
  if (stateName === "disarmed") {
    await page.waitForSelector('[data-testid="open-positions"] table', { timeout: 10_000 }).catch(() => {});
  }
  // Let the 250 ms flush cadence, sparkline and fonts settle.
  await page.waitForTimeout(1_400);

  const target = page.locator(selector).first();
  await target.screenshot({ path: join(OUT_DIR, outFile) });
  await context.close();
  console.log(`captured ${outFile}`);
}

const server = await serveDist();
mkdirSync(OUT_DIR, { recursive: true });
const browser = await chromium.launch();

try {
  await capture(browser, "engaged", "operator-console-engaged.png", ".operator-console");
  await capture(browser, "disarmed", "operator-console-disarmed.png", ".operator-console");
  await capture(browser, "disarmed", "live-stream-overview.png", "#live");
} finally {
  await browser.close();
  server.close();
}
