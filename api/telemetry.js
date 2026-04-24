const { resolveCountry } = require("./_distribution");

module.exports = async function handler(req, res) {
  if (req.method !== "POST") {
    res.setHeader("Allow", "POST");
    return res.status(405).json({ error: "Method not allowed" });
  }

  const payload = typeof req.body === "string" ? safeParse(req.body) : req.body || {};
  const resolvedCountry = await resolveCountry(req);
  const event = {
    ...payload,
    country: resolvedCountry.country || payload.country || "unknown",
    country_source: resolvedCountry.source || "unknown",
    received_at: new Date().toISOString(),
  };

  if (process.env.TELEMETRY_WEBHOOK_URL) {
    try {
      await fetch(process.env.TELEMETRY_WEBHOOK_URL, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(event),
      });
    } catch (err) {
      return res.status(502).json({ ok: false, error: String(err) });
    }
  } else {
    console.log("[telemetry]", JSON.stringify(event));
  }

  return res.status(202).json({ ok: true });
};

function safeParse(raw) {
  try {
    return JSON.parse(raw);
  } catch (_) {
    return {};
  }
}
