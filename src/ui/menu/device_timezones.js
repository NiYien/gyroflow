// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

.pragma library

// Each choice carries an IANA tz id; the actual UTC offset is computed at
// call time by the Rust side (controller.timezone_offset_minutes), so DST is
// always reflected. offsetMinutes is only the fallback for builds/entries
// whose tz id cannot be resolved, and the anchor for legacy-settings
// migration. A Rust guard test parses this file and rejects unresolvable ids.
var timezoneRegions = [
    { x: 0.0615, y: 0.3816, choices: [ { key: "Pago Pago", offsetMinutes: -660, tzId: "Pacific/Pago_Pago" }, { key: "Honolulu", offsetMinutes: -600, tzId: "Pacific/Honolulu" }, { key: "Taiohae", offsetMinutes: -570, tzId: "Pacific/Marquesas" }, { key: "Anchorage", offsetMinutes: -540, tzId: "America/Anchorage" } ] },
    { x: 0.1715, y: 0.3108, choices: [ { key: "Los Angeles", offsetMinutes: -480, tzId: "America/Los_Angeles" }, { key: "San Francisco", offsetMinutes: -480, tzId: "America/Los_Angeles" }, { key: "Vancouver", offsetMinutes: -480, tzId: "America/Vancouver" }, { key: "Denver", offsetMinutes: -420, tzId: "America/Denver" }, { key: "Phoenix", offsetMinutes: -420, tzId: "America/Phoenix" } ] },
    { x: 0.2566, y: 0.2673, choices: [ { key: "Chicago", offsetMinutes: -360, tzId: "America/Chicago" }, { key: "Mexico City", offsetMinutes: -360, tzId: "America/Mexico_City" } ] },
    { x: 0.2944, y: 0.2738, choices: [ { key: "New York", offsetMinutes: -300, tzId: "America/New_York" }, { key: "Toronto", offsetMinutes: -300, tzId: "America/Toronto" }, { key: "Caracas", offsetMinutes: -240, tzId: "America/Caracas" }, { key: "Halifax", offsetMinutes: -240, tzId: "America/Halifax" }, { key: "St. Johns", offsetMinutes: -210, tzId: "America/St_Johns" } ] },
    { x: 0.3705, y: 0.6308, choices: [ { key: "Sao Paulo", offsetMinutes: -180, tzId: "America/Sao_Paulo" }, { key: "Buenos Aires", offsetMinutes: -180, tzId: "America/Argentina/Buenos_Aires" }, { key: "Fernando de Noronha", offsetMinutes: -120, tzId: "America/Noronha" }, { key: "Praia", offsetMinutes: -60, tzId: "Atlantic/Cape_Verde" }, { key: "Ponta Delgada", offsetMinutes: -60, tzId: "Atlantic/Azores" } ] },
    { x: 0.4996, y: 0.2138, choices: [ { key: "London", offsetMinutes: 0, tzId: "Europe/London" }, { key: "Lisbon", offsetMinutes: 0, tzId: "Europe/Lisbon" } ] },
    { x: 0.5372, y: 0.2082, choices: [ { key: "Berlin", offsetMinutes: 60, tzId: "Europe/Berlin" }, { key: "Paris", offsetMinutes: 60, tzId: "Europe/Paris" } ] },
    { x: 0.5868, y: 0.3331, choices: [ { key: "Cairo", offsetMinutes: 120, tzId: "Africa/Cairo" }, { key: "Johannesburg", offsetMinutes: 120, tzId: "Africa/Johannesburg" } ] },
    { x: 0.6045, y: 0.1902, choices: [ { key: "Moscow", offsetMinutes: 180, tzId: "Europe/Moscow" }, { key: "Istanbul", offsetMinutes: 180, tzId: "Europe/Istanbul" }, { key: "Tehran", offsetMinutes: 210, tzId: "Asia/Tehran" } ] },
    { x: 0.6535, y: 0.3600, choices: [ { key: "Dubai", offsetMinutes: 240, tzId: "Asia/Dubai" }, { key: "Abu Dhabi", offsetMinutes: 240, tzId: "Asia/Dubai" }, { key: "Kabul", offsetMinutes: 270, tzId: "Asia/Kabul" }, { key: "Karachi", offsetMinutes: 300, tzId: "Asia/Karachi" }, { key: "Tashkent", offsetMinutes: 300, tzId: "Asia/Tashkent" } ] },
    { x: 0.7142, y: 0.3405, choices: [ { key: "Delhi", offsetMinutes: 330, tzId: "Asia/Kolkata" }, { key: "Mumbai", offsetMinutes: 330, tzId: "Asia/Kolkata" }, { key: "Kathmandu", offsetMinutes: 345, tzId: "Asia/Kathmandu" } ] },
    { x: 0.7792, y: 0.4236, choices: [ { key: "Dhaka", offsetMinutes: 360, tzId: "Asia/Dhaka" }, { key: "Thimphu", offsetMinutes: 360, tzId: "Asia/Thimphu" }, { key: "Yangon", offsetMinutes: 390, tzId: "Asia/Yangon" }, { key: "Bangkok", offsetMinutes: 420, tzId: "Asia/Bangkok" }, { key: "Jakarta", offsetMinutes: 420, tzId: "Asia/Jakarta" } ] },
    { x: 0.8374, y: 0.3265, choices: [ { key: "Shanghai", offsetMinutes: 480, tzId: "Asia/Shanghai" }, { key: "Beijing", offsetMinutes: 480, tzId: "Asia/Shanghai" }, { key: "Tianjin", offsetMinutes: 480, tzId: "Asia/Shanghai" }, { key: "Eucla", offsetMinutes: 525, tzId: "Australia/Eucla" } ] },
    { x: 0.8880, y: 0.3017, choices: [ { key: "Tokyo", offsetMinutes: 540, tzId: "Asia/Tokyo" }, { key: "Seoul", offsetMinutes: 540, tzId: "Asia/Seoul" }, { key: "Adelaide", offsetMinutes: 570, tzId: "Australia/Adelaide" }, { key: "Darwin", offsetMinutes: 570, tzId: "Australia/Darwin" } ] },
    { x: 0.9200, y: 0.6882, choices: [ { key: "Sydney", offsetMinutes: 600, tzId: "Australia/Sydney" }, { key: "Melbourne", offsetMinutes: 600, tzId: "Australia/Melbourne" }, { key: "Lord Howe", offsetMinutes: 630, tzId: "Australia/Lord_Howe" }, { key: "Noumea", offsetMinutes: 660, tzId: "Pacific/Noumea" }, { key: "Honiara", offsetMinutes: 660, tzId: "Pacific/Guadalcanal" } ] },
    { x: 0.9855, y: 0.7047, choices: [ { key: "Auckland", offsetMinutes: 720, tzId: "Pacific/Auckland" }, { key: "Wellington", offsetMinutes: 720, tzId: "Pacific/Auckland" }, { key: "Chatham", offsetMinutes: 765, tzId: "Pacific/Chatham" }, { key: "Nuku'alofa", offsetMinutes: 780, tzId: "Pacific/Tongatapu" }, { key: "Apia", offsetMinutes: 780, tzId: "Pacific/Apia" }, { key: "Kiritimati", offsetMinutes: 840, tzId: "Pacific/Kiritimati" } ] }
]

// Reverse lookup used for legacy-settings migration (pre-tzId installs only
// stored the city key). "System" passes through as the follow-the-system
// marker; unknown keys return "" so callers fall back to the system offset.
function tzIdForCityKey(key) {
    if (!key)
        return ""
    if (key === "System")
        return "System"
    for (var i = 0; i < timezoneRegions.length; ++i) {
        var choices = timezoneRegions[i].choices
        for (var j = 0; j < choices.length; ++j) {
            if (choices[j].key === key)
                return choices[j].tzId
        }
    }
    return ""
}
