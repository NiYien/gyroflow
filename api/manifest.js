const path = require("path");
const {
  buildDownloadApiUrl,
  buildManualVersionEntry,
  buildPlatformPackage,
  buildReleaseAssetUrl,
  getDownloadApiBase,
  loadDistributionConfig,
  loadReleasePolicy,
  normalizePlatform,
  packageMapForPlatform,
  readJsonIfExists,
  resolveCountry,
  selectSource,
  toAbsoluteManifestUrl,
} = require("./_distribution");

function envFlagEnabled(name) {
  return ["1", "true", "yes", "on"].includes(
    String(process.env[name] || "").trim().toLowerCase()
  );
}

function buildDownloadApiDirectory(req, scope, tag) {
  if (!scope || !tag) {
    return "";
  }
  const encodedTag = String(tag)
    .trim()
    .split("/")
    .filter((item) => item)
    .map((item) => encodeURIComponent(item))
    .join("/");
  return encodedTag ? `${getDownloadApiBase(req)}/${scope}/${encodedTag}/` : "";
}

module.exports = async function handler(req, res) {
  const config = loadDistributionConfig();
  const releasePolicy = loadReleasePolicy();
  const resolvedCountry = await resolveCountry(req);
  const country = resolvedCountry.country;
  const source = selectSource(config, country);
  const platform = normalizePlatform(req.query.platform || "windows");

  const lensMeta =
    readJsonIfExists(
      path.join(process.cwd(), "_deployment", "_binaries", "gyroflow-niyien-lens.cbor.gz.json")
    ) || {};

  const autoEntry =
    releasePolicy.versions.find((item) => item.version === releasePolicy.auto_version) ||
    releasePolicy.versions[0];
  const manualVersions = releasePolicy.versions
    .filter((item) => item.channels.includes("manual"))
    .map((item) => {
      const manualPackage = buildPlatformPackage(req, item, source, platform);
      const manualPackages = packageMapForPlatform(platform, manualPackage);
      return buildManualVersionEntry(item, manualPackage, manualPackages);
    });
  const platformPackage = buildPlatformPackage(req, autoEntry, source, platform);
  const appPackages = Object.keys(platformPackage).length ? { [platform]: platformPackage } : {};
  // Keep app.url byte-equal with app.packages.<platform>.installer_url/package_url.
  const appUrl = toAbsoluteManifestUrl(
    req,
    platformPackage.installer_url || platformPackage.package_url || ""
  );
  const activeTag = autoEntry ? autoEntry.tag : `v${releasePolicy.auto_version}`;
  const lensDisabled = envFlagEnabled("NIYIEN_LENS_DISABLED");
  const pluginsDisabled = envFlagEnabled("NIYIEN_PLUGINS_DISABLED");
  const sdkDisabled = envFlagEnabled("NIYIEN_SDK_DISABLED");
  // Decoupled bundle layout: lens lives in `releases/lens-<sha12>/` and plugin
  // in `releases/plugin-<sha12>/`. legacy fallback: when the new tags are
  // missing, fall back to the coupled `releases/<content_tag>/[plugins/]`
  // layout so older policy entries keep working.
  const legacyContentTag =
    autoEntry?.content_tag ||
    process.env.NIYIEN_CONTENT_RELEASE_TAG ||
    process.env.NIYIEN_DATA_RELEASE_TAG ||
    activeTag;
  const lensTag =
    lensDisabled
      ? ""
      : autoEntry?.lens_tag ||
        process.env.NIYIEN_LENS_RELEASE_TAG ||
        legacyContentTag;
  const pluginTag =
    pluginsDisabled
      ? ""
      : autoEntry?.plugin_tag ||
        process.env.NIYIEN_PLUGIN_RELEASE_TAG ||
        (legacyContentTag ? `${legacyContentTag}/plugins` : "");
  const legacyLensVersion =
    process.env.NIYIEN_WIDE_LENS_VERSION ||
    process.env.NIYIEN_CAMERA_DB_VERSION ||
    "";
  const legacyLensSha =
    process.env.NIYIEN_WIDE_LENS_SHA256 ||
    process.env.NIYIEN_CAMERA_DB_SHA256 ||
    "";
  const lensAssetName = config.data.lens.asset_name;
  const lensUrl =
    lensDisabled
      ? ""
      : source.region === "cn"
      ? buildDownloadApiUrl(req, "content", lensTag, lensAssetName)
      : buildReleaseAssetUrl(source.base, activeTag, lensAssetName);
  const sdkBase =
    sdkDisabled
      ? ""
      : source.region === "cn"
        ? `${getDownloadApiBase(req)}/content/sdk/`
        : autoEntry?.global_sdk_base ||
          process.env.NIYIEN_GLOBAL_SDK_BASE ||
          process.env.NIYIEN_SDK_BASE ||
          "https://www.niyien.com/api/sdk/";
  const pluginsBase =
    pluginsDisabled
      ? ""
      : source.region === "cn"
        ? buildDownloadApiDirectory(req, "content", pluginTag)
        : autoEntry?.global_plugins_base || `${source.base}/${pluginTag}/`;

  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.status(200).json({
    country,
    country_source: resolvedCountry.source,
    region: source.region,
    app: {
      version: autoEntry ? autoEntry.version : "",
      url: appUrl,
      changelog: autoEntry ? autoEntry.changelog : "",
      manual_versions: manualVersions,
      packages: appPackages,
    },
    lens: {
      version: lensDisabled
        ? 0
        : Number(
          autoEntry?.lens_version ||
          process.env.NIYIEN_LENS_VERSION ||
          legacyLensVersion ||
          lensMeta.version ||
          1
        ),
      url: toAbsoluteManifestUrl(req, lensUrl),
      sha256: lensDisabled
        ? ""
        : autoEntry?.lens_sha256 ||
          process.env.NIYIEN_LENS_SHA256 ||
          legacyLensSha ||
          lensMeta.sha256 ||
          "",
    },
    // SDK is shared across releases — uploaded to a flat `releases/sdk/`
    // directory by publish_pan123_release.py (since the decoupling change),
    // so its URL does NOT include the per-release content_tag. Filenames
    // carry their version (e.g. `RED_SDK_Windows_9.1.2.tar.gz`), so a
    // newer SDK shows up as a new filename without invalidating older
    // clients that still ask for the old filename.
    sdk_base: toAbsoluteManifestUrl(req, sdkBase),
    plugins_base: toAbsoluteManifestUrl(req, pluginsBase),
  });
};
