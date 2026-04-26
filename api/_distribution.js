const fs = require("fs");
const path = require("path");

const COUNTRY_CACHE_TTL_MS = 6 * 60 * 60 * 1000;
const DEFAULT_DOWNLOAD_API_BASE = "https://www.niyien.com/api/download";
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
  const region = cnCountries.includes(country) ? "cn" : "global";
  const source = region === "cn" ? config.sources.cn : config.sources.global;
  return {
    ...source,
    region,
    selectedSource: region,
  };
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
  if (!Array.isArray(channels) || !channels.length) return ["manual"];
  const values = Array.from(
    new Set(
      channels
        .map((item) => String(item || "").trim().toLowerCase())
        .filter((item) => item === "auto" || item === "manual")
    )
  );
  return values.length ? values : ["manual"];
}

function normalizePolicyEntry(entry) {
  if (!entry || typeof entry !== "object") return null;
  const version = String(entry.version || "").trim();
  const tag = String(entry.tag || (version ? `v${version}` : "")).trim();
  if (!version || !tag) return null;
  return {
    version,
    tag,
    channels: normalizeChannels(entry.channels),
    changelog: typeof entry.changelog === "string" ? entry.changelog.trim() : "",
    recommended: Boolean(entry.recommended),
    app_source_mode:
      typeof entry.app_source_mode === "string" && entry.app_source_mode.trim()
        ? entry.app_source_mode.trim().toLowerCase()
        : "release",
    app_urls: normalizeAppUrls(entry.app_urls),
    packages: normalizePackages(entry.packages),
    content_tag: typeof entry.content_tag === "string" ? entry.content_tag.trim() : "",
    plugins_source_mode:
      typeof entry.plugins_source_mode === "string" && entry.plugins_source_mode.trim()
        ? entry.plugins_source_mode.trim().toLowerCase()
        : "",
    plugins_source_ref:
      typeof entry.plugins_source_ref === "string" ? entry.plugins_source_ref.trim() : "",
    plugins_source_tag:
      typeof entry.plugins_source_tag === "string" ? entry.plugins_source_tag.trim() : "",
    global_plugins_base:
      typeof entry.global_plugins_base === "string" ? entry.global_plugins_base.trim() : "",
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

function normalizePlatform(value) {
  const platform = String(value || "").trim().toLowerCase();
  if (platform === "macos" || platform === "linux" || platform === "android") {
    return platform;
  }
  return "windows";
}

function appAssetName(platform) {
  switch (normalizePlatform(platform)) {
    case "macos":
      return "gyroflow-niyien-mac-universal.dmg";
    case "linux":
      return "gyroflow-niyien-linux64.AppImage";
    case "android":
      return "gyroflow-niyien.apk";
    case "windows":
    default:
      return "gyroflow-niyien-windows64-setup.exe";
  }
}

function appPackageAssetName(platform) {
  switch (normalizePlatform(platform)) {
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

function appInstallerAssetName(platform) {
  return normalizePlatform(platform) === "windows" ? "gyroflow-niyien-windows64-setup.exe" : "";
}

function buildAppUrl(sourceBase, tag, platform) {
  return buildReleaseAssetUrl(sourceBase, tag, appAssetName(platform));
}

function buildReleaseAssetUrl(sourceBase, tag, filename) {
  if (!sourceBase || !tag || !filename) {
    return "";
  }
  return `${stripTrailingSlash(sourceBase)}/${tag}/${filename}`;
}

function normalizeAppUrls(value) {
  if (!value || typeof value !== "object") {
    return {};
  }
  const result = {};
  for (const [platform, rawValue] of Object.entries(value)) {
    const key = normalizePlatform(platform);
    if (typeof rawValue === "string") {
      const packageUrl = rawValue.trim();
      if (packageUrl) {
        result[key] = { package_url: packageUrl };
      }
      continue;
    }
    if (rawValue && typeof rawValue === "object") {
      const installerUrl = String(rawValue.installer_url || "").trim();
      const packageUrl = String(rawValue.package_url || rawValue.url || "").trim();
      if (installerUrl || packageUrl) {
        result[key] = {};
        if (installerUrl) result[key].installer_url = installerUrl;
        if (packageUrl) result[key].package_url = packageUrl;
      }
    }
  }
  return result;
}

function normalizePackages(value) {
  if (!value || typeof value !== "object") {
    return {};
  }
  const result = {};
  for (const [platform, rawValue] of Object.entries(value)) {
    const key = normalizePlatform(platform);
    if (!rawValue || typeof rawValue !== "object") {
      continue;
    }
    result[key] = {
      kind: String(rawValue.kind || defaultPackageKind(key)).trim(),
      installer_filename: String(rawValue.installer_filename || "").trim(),
      installer_sha256: String(rawValue.installer_sha256 || "").trim().toLowerCase(),
      installer_size: coercePositiveInteger(rawValue.installer_size),
      package_filename: String(rawValue.package_filename || "").trim(),
      package_sha256: String(rawValue.package_sha256 || "").trim().toLowerCase(),
      package_size: coercePositiveInteger(rawValue.package_size),
    };
  }
  return result;
}

function buildPlatformPackage(req, entry, source, platform) {
  const key = normalizePlatform(platform);
  if (!entry) {
    return {};
  }

  const metadata = entry.packages?.[key] || {};
  const urls = resolvePlatformPackageUrls(req, entry, source, key, metadata);

  if (key === "windows") {
    return withAbsolutePackageUrls(req, {
      kind: metadata.kind || "web_installer_zip",
      installer_url: urls.installer_url || "",
      installer_sha256: metadata.installer_sha256 || "",
      installer_size: metadata.installer_size || 0,
      package_url: urls.package_url || "",
      package_sha256: metadata.package_sha256 || "",
      package_size: metadata.package_size || 0,
    });
  }

  return withAbsolutePackageUrls(req, {
    kind: metadata.kind || defaultPackageKind(key),
    package_url: urls.package_url || "",
    package_sha256: metadata.package_sha256 || "",
    package_size: metadata.package_size || 0,
  });
}

function resolvePlatformPackageUrls(req, entry, source, platform, metadata) {
  if (!entry?.tag) {
    return {};
  }

  if (source.region === "cn") {
    return {
      installer_url: appInstallerAssetName(platform)
        ? buildDownloadApiUrl(req, "app", entry.tag, metadata.installer_filename || appInstallerAssetName(platform))
        : "",
      package_url: buildDownloadApiUrl(
        req,
        "app",
        entry.tag,
        metadata.package_filename || appPackageAssetName(platform)
      ),
    };
  }

  if (String(entry.app_source_mode || "").trim().toLowerCase() === "artifact") {
    const artifactUrls = entry.app_urls?.[platform] || {};
    return {
      installer_url: artifactUrls.installer_url || "",
      package_url: artifactUrls.package_url || "",
    };
  }

  return {
    installer_url: appInstallerAssetName(platform)
      ? buildReleaseAssetUrl(source.base, entry.tag, metadata.installer_filename || appInstallerAssetName(platform))
      : "",
    package_url: buildReleaseAssetUrl(
      source.base,
      entry.tag,
      metadata.package_filename || appPackageAssetName(platform)
    ),
  };
}

function withAbsolutePackageUrls(req, platformPackage) {
  if (!platformPackage || typeof platformPackage !== "object") {
    return {};
  }
  const result = { ...platformPackage };
  if ("installer_url" in result) {
    result.installer_url = toAbsoluteManifestUrl(req, result.installer_url || "");
  }
  if ("package_url" in result) {
    result.package_url = toAbsoluteManifestUrl(req, result.package_url || "");
  }
  return result;
}

function defaultPackageKind(platform) {
  return normalizePlatform(platform) === "windows" ? "web_installer_zip" : "dmg";
}

function coercePositiveInteger(value) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric) || numeric <= 0) {
    return 0;
  }
  return Math.trunc(numeric);
}

function stripTrailingSlash(value) {
  return String(value || "").trim().replace(/\/+$/, "");
}

function getRequestOrigin(req) {
  const host = String(req?.headers?.host || "").trim();
  const protocol = String(req?.headers?.["x-forwarded-proto"] || "https")
    .split(",")[0]
    .trim();
  if (!host) {
    return "https://www.niyien.com";
  }
  return `${protocol || "https"}://${host}`;
}

function getManifestUrlOrigin(req) {
  const envBase = stripTrailingSlash(process.env.NIYIEN_DOWNLOAD_API_BASE || "");
  if (envBase) {
    try {
      return new URL(envBase).origin;
    } catch (_) {}
  }
  return stripTrailingSlash(getRequestOrigin(req));
}

function getDownloadApiBase(req) {
  const envBase = stripTrailingSlash(process.env.NIYIEN_DOWNLOAD_API_BASE || "");
  if (envBase) {
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(envBase)) {
      return envBase;
    }
    const origin = getManifestUrlOrigin(req);
    if (envBase.startsWith("/")) {
      return `${origin}${envBase}`;
    }
    return `${origin}/${envBase.replace(/^\/+/, "")}`;
  }
  return DEFAULT_DOWNLOAD_API_BASE;
}

function toAbsoluteManifestUrl(req, value) {
  const raw = String(value || "").trim();
  if (!raw) {
    return "";
  }
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
    return raw;
  }

  const origin = getManifestUrlOrigin(req);
  if (raw.startsWith("/api/download/") || raw.startsWith("/")) {
    return `${origin}${raw}`;
  }
  return `${getDownloadApiBase(req)}/${raw.replace(/^\/+/, "")}`;
}

function buildDownloadApiUrl(req, scope, tag, relativePath) {
  if (!scope || !tag || !relativePath) {
    return "";
  }
  const encodedTag = encodeURIComponent(String(tag).trim());
  const encodedPath = String(relativePath)
    .split("/")
    .map((item) => encodeURIComponent(String(item)))
    .join("/");
  return `${getDownloadApiBase(req)}/${scope}/${encodedTag}/${encodedPath}`;
}

module.exports = {
  appInstallerAssetName,
  appAssetName,
  appPackageAssetName,
  buildDownloadApiUrl,
  buildAppUrl,
  buildPlatformPackage,
  buildReleaseAssetUrl,
  getDefaultAppVersion,
  getDownloadApiBase,
  loadDistributionConfig,
  loadReleasePolicy,
  normalizePlatform,
  readJsonIfExists,
  getCountry,
  resolveCountry,
  selectSource,
  toAbsoluteManifestUrl,
};
