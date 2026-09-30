import http from "k6/http";
import { check, sleep } from "k6";
import ws from "k6/ws";

const baseUrl = __ENV.BASE_URL || "http://127.0.0.1:8081";
const loadedTokens = (__ENV.TRADER_TOKENS || "").split(",").filter(Boolean);
const vus = Number(__ENV.K6_VUS || 10);
const duration = __ENV.K6_DURATION || "2m";

export const options = {
  scenarios: {
    market_browsing: {
      executor: "ramping-vus",
      exec: "browseMarket",
      startVUs: 0,
      stages: [
        { duration: "5s", target: vus },
        { duration, target: vus },
        { duration: "5s", target: 0 },
      ],
      gracefulRampDown: "10s",
    },
    trader_sessions: {
      executor: "constant-vus",
      exec: "trade",
      vus: Math.max(1, Math.floor(vus / 3)),
      duration,
      startTime: "10s",
    },
    websocket_subscribers: {
      executor: "constant-vus",
      exec: "subscribe",
      vus: Math.max(1, Math.floor(vus / 4)),
      duration,
      startTime: "10s",
    },
    alert_heavy: {
      executor: "constant-vus",
      exec: "manageAlerts",
      vus: Math.max(1, Math.floor(vus / 3)),
      duration,
      startTime: "10s",
    },
  },
  thresholds: {
    http_req_failed: ["rate<0.01"],
    http_req_duration: ["p(95)<750", "p(99)<1500"],
    checks: ["rate>0.99"],
    ws_connecting: ["p(95)<1000"],
  },
};

function request(method, path, body, token) {
  const headers = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  return http.request(method, `${baseUrl}${path}`, body ? JSON.stringify(body) : null, { headers });
}

function assertOk(response, label) {
  check(response, { [`${label} succeeds`]: (r) => r.status >= 200 && r.status < 300 });
}

export function browseMarket() {
  const mix = [
    () => http.get(`${baseUrl}/api/v1/spot`),
    () => http.get(`${baseUrl}/api/v1/chain?underlying=BTC&expiry_days=30`),
    () => http.get(`${baseUrl}/api/v1/expiries/BTC`),
    () => http.get(`${baseUrl}/api/v1/stats`),
  ];
  assertOk(mix[Math.floor(Math.random() * mix.length)](), "market data");
  sleep(Math.random() * 2 + 0.5);
}

function sessionToken() {
  if (!loadedTokens.length) {
    throw new Error("authenticated scenarios require TRADER_TOKENS (one bearer token per VU)");
  }
  return loadedTokens[(__VU - 1) % loadedTokens.length];
}

export function trade() {
  const token = sessionToken();
  const headers = { Authorization: `Bearer ${token}`, "Content-Type": "application/json" };
  const opened = http.post(
    `${baseUrl}/api/v1/positions/open`,
    JSON.stringify({ underlying: "BTC", strike: 70000, expiry_days: 30, option_type: "call", position_type: "long", contracts: 1 }),
    { headers },
  );
  assertOk(opened, "open position");
  if (opened.status < 200 || opened.status >= 300) return;
  const positionId = opened.json("id");

  const rolled = request("POST", `/api/v1/positions/${positionId}/roll`, { new_strike: 71000, new_expiry_days: 45 }, token);
  assertOk(rolled, "roll position");
  if (rolled.status >= 200 && rolled.status < 300) {
    const replacementId = rolled.json("opened.id");
    assertOk(request("POST", `/api/v1/positions/${replacementId}/close`, {}, token), "close position");
  }

  const strategy = request("POST", "/api/v1/strategies/execute", {
    legs: [
      { underlying: "BTC", strike: 68000, expiry_days: 30, option_type: "call", position_type: "long", contracts: 1 },
      { underlying: "BTC", strike: 72000, expiry_days: 30, option_type: "call", position_type: "short", contracts: 1 },
    ],
  }, token);
  assertOk(strategy, "open strategy");
  if (strategy.status >= 200 && strategy.status < 300) {
    assertOk(request("POST", `/api/v1/strategies/${strategy.json("0.strategy_id")}/close`, {}, token), "close strategy");
  }
  sleep(1);
}

export function subscribe() {
  const response = ws.connect(`${baseUrl.replace(/^http/, "ws")}/api/v1/ws/spot`, {}, (socket) => {
    socket.on("message", (message) => check(message, { "spot snapshot received": (value) => value.length > 0 }));
    socket.setTimeout(() => socket.close(), 5000);
  });
  check(response, { "websocket connected": (r) => r && r.status === 101 });
  sleep(1);
}

export function manageAlerts() {
  const token = sessionToken();
  const created = request("POST", "/api/v1/alerts", {
    underlying: "BTC",
    condition: Math.random() < 0.5 ? "above" : "below",
    target_price: 65000 + Math.floor(Math.random() * 10000),
  }, token);
  assertOk(created, "create alert");
  if (created.status >= 200 && created.status < 300) {
    const alertId = created.json("id");
    assertOk(request("DELETE", `/api/v1/alerts/${alertId}`, null, token), "delete alert");
  }
  assertOk(request("GET", "/api/v1/alerts", null, token), "list alerts");
  sleep(0.5);
}
