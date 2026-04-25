const fs = require("fs");
const path = require("path");

const COUNTRY_CACHE_TTL_MS = 6 * 60 * 60 * 1000;
const countryCache = new Map();
const pendingCountryLookups = new Map();

function parseTomlValue(raw) {
  const value = raw.trim();
  if (value.startsWith('"') && value.endsWith('"')) {
    return value.slice(1, -1);
  }
  if (value.startsWith("[") && value.endsWith("]")) {
    const inner = value.slice(1, -1).trim();
    if (!inner) return [];
    return inner.split(",").map((item) => parseTomlValue(item));
  }
  if (/^\d+$/.test(value)) {
    return Number(value);
  }
  return value;
}

function parseToml(text) {
  const root = {};
  let current = root;
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
      const parts = trimmed.slice(1, -1).split(".");
      current = root;
      for (const part of parts) {
        current[part] = current[part] || {};
        current = current[part];
      }
      continue;
    }
    const index = trimmed.indexOf("=");
    if (index === -1) continue;
    const key = trimmed.slice(0, index).trim();
    const value = trimmed.slice(index + 1);
    current[key] = parseTomlValue(value);
  }
  return root;
}

function loadDistributionConfig() {
  const file = path.join(process.cwd(), "distribution", "niyien.toml");
  return parseToml(fs.readFileSync(file, "utf8"));
}

function readJsonIfExists(filePath) {
  if (!fs.existsSync(filePath)) return null;
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function getClientIp(req) {
  const cfConnectingIp = req?.headers?.["cf-connecting-ip"];
  if (typeof cfConnectingIp === "string" && cfConnectingIp.trim()) {
    return cfConnectingIp.trim();
  }

  const trueClientIp = req?.headers?.["true-client-ip"];
  if (typeof trueClientIp === "string" && trueClientIp.trim()) {
    return trueClientIp.trim();
  }

  const forwarded = req?.headers?.["x-forwarded-for"];
  if (typeof forwarded === "string" && forwarded.trim()) {
    return forwarded.split(",")[0].trim();
  }

  return req?.socket?.remoteAddress || "";
}

function normalizeCountry(value) {
  return typeof value === "string" ? value.trim().toUpperCase() : "";
}

function getCachedCountry(ip) {
  if (!ip) {
    return null;
  }
  const cached = countryCache.get(ip);
  if (!cached) {
    return null;
  }
  if (cached.expires_at <= Date.now()) {
    countryCache.delete(ip);
    return null;
  }
  return cached;
}

function setCachedCountry(ip, country, source, ttlMs = COUNTRY_CACHE_TTL_MS) {
  if (!ip || !country) {
    return;
  }
  countryCache.set(ip, {
    country,
    source,
    expires_at: Date.now() + ttlMs,
  });
}

async function lookupCountryByIpInfo(ip) {
  const token = String(process.env.IPINFO_TOKEN || "").trim();
  if (!token || !ip) {
    return "";
  }

  const url = `https://ipinfo.io/${encodeURIComponent(ip)}?token=${encodeURIComponent(token)}`;
  try {
    const response = await fetch(url, { method: "GET" });
    if (!response.ok) {
      return "";
    }
    const data = await response.json();
    return normalizeCountry(data.country);
  } catch (_) {
    return "";
  }
}

async function resolveCountry(req) {
  const queryCountry = normalizeCountry(req?.query?.country);
  if (queryCountry) {
    return { country: queryCountry, source: "query" };
  }

  const headerCountry = normalizeCountry(req?.headers?.["x-country-code"]);
  if (headerCountry) {
    return { country: headerCountry, source: "header" };
  }

  const clientIp = getClientIp(req);
  const cached = getCachedCountry(clientIp);
  if (cached) {
    return { country: cached.country, source: cached.source };
  }

  if (clientIp) {
    let pending = pendingCountryLookups.get(clientIp);
    if (!pending) {
      pending = lookupCountryByIpInfo(clientIp)
        .then((country) => {
          const normalized = normalizeCountry(country);
          if (normalized) {
            setCachedCountry(clientIp, normalized, "ipinfo");
          }
          return normalized;
        })
        .finally(() => {
          pendingCountryLookups.delete(clientIp);
        });
      pendingCountryLookups.set(clientIp, pending);
    }
    const ipInfoCountry = await pending;
    if (ipInfoCountry) {
      return { country: ipInfoCountry, source: "ipinfo" };
    }
  }

  const vercelCountry = normalizeCountry(req?.headers?.["x-vercel-ip-country"]);
  if (vercelCountry) {
    setCachedCountry(clientIp, vercelCountry, "vercel", 30 * 60 * 1000);
    return { country: vercelCountry, source: "vercel" };
  }

  setCachedCountry(clientIp, "US", "default", 30 * 60 * 1000);
  return { country: "US", source: "default" };
}

async function getCountry(req) {
  const resolved = await resolveCountry(req);
  return resolved.country;
}

function selectSource(config, country) {
  const cnCountries = config.routing?.cn_countries || [];
  return cnCountries.includes(country) ? config.sources.cn : config.sources.global;
}

function getDefaultAppVersion() {
  // Canonical format matches build.rs:57-83 (distribution/control_center plan):
  //   Tag build          →  "1.6.3"
  //   Action build N     →  "1.6.3-0.ni.N"
  // Keep -0.ni.1 as the fallback so has_app_update() in distribution.rs
  // has a valid prerelease that compares lower than a real tagged release.
  return process.env.NIYIEN_APP_VERSION || `${process.env.npm_package_version || "1.6.3"}-0.ni.1`;
}

function getDefaultReleaseTag(version) {
  return process.env.NIYIEN_RELEASE_TAG || `v${version}`;
}

function normalizeChannels(channels) {
  if (!Array.isArray(channels)) return ["manual"];
  return channels.filter((item) => typeof item === "string" && item.length > 0);
}

function normalizePolicyEntry(entry) {
  if (!entry || typeof entry !== "object") return null;
  if (!entry.version || !entry.tag) return null;
  return {
    version: String(entry.version),
    tag: String(entry.tag),
    channels: normalizeChannels(entry.channels),
    changelog: typeof entry.changelog === "string" ? entry.changelog : "",
    recommended: Boolean(entry.recommended),
  };
}

function loadReleasePolicy() {
  const fallbackVersion = getDefaultAppVersion();
  const fallbackTag = getDefaultReleaseTag(fallbackVersion);
  const fallback = {
    auto_version: fallbackVersion,
    versions: [
      {
        version: fallbackVersion,
        tag: fallbackTag,
        channels: ["auto", "manual"],
        changelog: process.env.NIYIEN_APP_CHANGELOG || "",
        recommended: true,
      },
    ],
  };

  const raw = process.env.NIYIEN_RELEASE_POLICY_JSON;
  if (!raw || !raw.trim()) {
    return fallback;
  }

  try {
    const parsed = JSON.parse(raw);
    const versions = Array.isArray(parsed.versions)
      ? parsed.versions.map(normalizePolicyEntry).filter(Boolean)
      : [];
    if (versions.length === 0) {
      return fallback;
    }
    const autoVersion = typeof parsed.auto_version === "string" && parsed.auto_version.length > 0
      ? parsed.auto_version
      : (versions.find((item) => item.channels.includes("auto")) || versions[0]).version;
    if (!versions.some((item) => item.version === autoVersion)) {
      return fallback;
    }
    return {
      auto_version: autoVersion,
      versions,
    };
  } catch (_) {
    return fallback;
  }
}

function appAssetName(platform) {
  switch (platform) {
    case "macos":
      return "gyroflow-niyien-mac-universal.dmg";
    case "linux":
      return "gyroflow-niyien-linux64.AppImage";
    case "android":
      return "gyroflow-niyien.apk";
    case "windows":
    default:
      return "gyroflow-niyien-windows64.zip";
  }
}

function buildAppUrl(sourceBase, tag, platform) {
  return `${sourceBase}/${tag}/${appAssetName(platform)}`;
}

module.exports = {
  appAssetName,
  buildAppUrl,
  getDefaultAppVersion,
  loadDistributionConfig,
  loadReleasePolicy,
  readJsonIfExists,
  getCountry,
  resolveCountry,
  selectSource,
};
