const path = require("path");
const {
  buildAppUrl,
  loadDistributionConfig,
  loadReleasePolicy,
  readJsonIfExists,
  resolveCountry,
  selectSource,
} = require("./_distribution");

module.exports = async function handler(req, res) {
  const config = loadDistributionConfig();
  const releasePolicy = loadReleasePolicy();
  const resolvedCountry = await resolveCountry(req);
  const country = resolvedCountry.country;
  const source = selectSource(config, country);
  const platform = (req.query.platform || "windows").toString();

  const lensMeta =
    readJsonIfExists(
      path.join(process.cwd(), "_deployment", "_binaries", "gyroflow-niyien-lens.cbor.gz.json")
    ) || {};

  const autoEntry =
    releasePolicy.versions.find((item) => item.version === releasePolicy.auto_version) ||
    releasePolicy.versions[0];
  const manualVersions = releasePolicy.versions
    .filter((item) => item.channels.includes("manual"))
    .map((item) => ({
      version: item.version,
      url: buildAppUrl(source.base, item.tag, platform),
      changelog: item.changelog,
      recommended: item.recommended,
    }));
  const activeTag = autoEntry ? autoEntry.tag : `v${releasePolicy.auto_version}`;
  const contentTag =
    process.env.NIYIEN_CONTENT_RELEASE_TAG ||
    process.env.NIYIEN_DATA_RELEASE_TAG ||
    activeTag;
  const legacyLensVersion =
    process.env.NIYIEN_WIDE_LENS_VERSION ||
    process.env.NIYIEN_CAMERA_DB_VERSION ||
    "";
  const legacyLensSha =
    process.env.NIYIEN_WIDE_LENS_SHA256 ||
    process.env.NIYIEN_CAMERA_DB_SHA256 ||
    "";

  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.status(200).json({
    country,
    country_source: resolvedCountry.source,
    region: country === "CN" ? "cn" : "global",
    app: {
      version: autoEntry ? autoEntry.version : "",
      url: autoEntry ? buildAppUrl(source.base, autoEntry.tag, platform) : "",
      changelog: autoEntry ? autoEntry.changelog : "",
      manual_versions: manualVersions,
    },
    lens: {
      version: Number(
        process.env.NIYIEN_LENS_VERSION || legacyLensVersion || lensMeta.version || 1
      ),
      url: `${source.base}/${contentTag}/${config.data.lens.asset_name}`,
      sha256: process.env.NIYIEN_LENS_SHA256 || legacyLensSha || lensMeta.sha256 || "",
    },
    sdk_base: `${source.base}/${contentTag}/sdk/`,
    plugins_base: `${source.base}/${contentTag}/plugins/`,
  });
};
