const fs = require("fs");
const path = require("path");

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

function getCountry(req) {
  return (
    req.headers["x-vercel-ip-country"] ||
    req.headers["x-country-code"] ||
    req.query.country ||
    "US"
  ).toString().toUpperCase();
}

function selectSource(config, country) {
  const cnCountries = config.routing?.cn_countries || [];
  return cnCountries.includes(country) ? config.sources.cn : config.sources.global;
}

function getDefaultAppVersion() {
  return process.env.NIYIEN_APP_VERSION || `${process.env.npm_package_version || "1.6.3"}-niyien.1`;
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
  selectSource,
};
