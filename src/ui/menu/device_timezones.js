// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

.pragma library

var timezoneRegions = [
    { x: 0.0615, y: 0.3816, choices: [ { key: "Pago Pago", offsetMinutes: -660 }, { key: "Honolulu", offsetMinutes: -600 }, { key: "Taiohae", offsetMinutes: -570 }, { key: "Anchorage", offsetMinutes: -540 } ] },
    { x: 0.1715, y: 0.3108, choices: [ { key: "Los Angeles", offsetMinutes: -480 }, { key: "San Francisco", offsetMinutes: -480 }, { key: "Vancouver", offsetMinutes: -480 }, { key: "Denver", offsetMinutes: -420 }, { key: "Phoenix", offsetMinutes: -420 } ] },
    { x: 0.2566, y: 0.2673, choices: [ { key: "Chicago", offsetMinutes: -360 }, { key: "Mexico City", offsetMinutes: -360 } ] },
    { x: 0.2944, y: 0.2738, choices: [ { key: "New York", offsetMinutes: -300 }, { key: "Toronto", offsetMinutes: -300 }, { key: "Caracas", offsetMinutes: -240 }, { key: "Halifax", offsetMinutes: -240 }, { key: "St. Johns", offsetMinutes: -210 } ] },
    { x: 0.3705, y: 0.6308, choices: [ { key: "Sao Paulo", offsetMinutes: -180 }, { key: "Buenos Aires", offsetMinutes: -180 }, { key: "Fernando de Noronha", offsetMinutes: -120 }, { key: "Praia", offsetMinutes: -60 }, { key: "Ponta Delgada", offsetMinutes: -60 } ] },
    { x: 0.4996, y: 0.2138, choices: [ { key: "London", offsetMinutes: 0 }, { key: "Lisbon", offsetMinutes: 0 } ] },
    { x: 0.5372, y: 0.2082, choices: [ { key: "Berlin", offsetMinutes: 60 }, { key: "Paris", offsetMinutes: 60 } ] },
    { x: 0.5868, y: 0.3331, choices: [ { key: "Cairo", offsetMinutes: 120 }, { key: "Johannesburg", offsetMinutes: 120 } ] },
    { x: 0.6045, y: 0.1902, choices: [ { key: "Moscow", offsetMinutes: 180 }, { key: "Istanbul", offsetMinutes: 180 }, { key: "Tehran", offsetMinutes: 210 } ] },
    { x: 0.6535, y: 0.3600, choices: [ { key: "Dubai", offsetMinutes: 240 }, { key: "Abu Dhabi", offsetMinutes: 240 }, { key: "Kabul", offsetMinutes: 270 }, { key: "Karachi", offsetMinutes: 300 }, { key: "Tashkent", offsetMinutes: 300 } ] },
    { x: 0.7142, y: 0.3405, choices: [ { key: "Delhi", offsetMinutes: 330 }, { key: "Mumbai", offsetMinutes: 330 }, { key: "Kathmandu", offsetMinutes: 345 } ] },
    { x: 0.7792, y: 0.4236, choices: [ { key: "Dhaka", offsetMinutes: 360 }, { key: "Thimphu", offsetMinutes: 360 }, { key: "Yangon", offsetMinutes: 390 }, { key: "Bangkok", offsetMinutes: 420 }, { key: "Jakarta", offsetMinutes: 420 } ] },
    { x: 0.8374, y: 0.3265, choices: [ { key: "Shanghai", offsetMinutes: 480 }, { key: "Beijing", offsetMinutes: 480 }, { key: "Tianjin", offsetMinutes: 480 }, { key: "Eucla", offsetMinutes: 525 } ] },
    { x: 0.8880, y: 0.3017, choices: [ { key: "Tokyo", offsetMinutes: 540 }, { key: "Seoul", offsetMinutes: 540 }, { key: "Adelaide", offsetMinutes: 570 }, { key: "Darwin", offsetMinutes: 570 } ] },
    { x: 0.9200, y: 0.6882, choices: [ { key: "Sydney", offsetMinutes: 600 }, { key: "Melbourne", offsetMinutes: 600 }, { key: "Lord Howe", offsetMinutes: 630 }, { key: "Noumea", offsetMinutes: 660 }, { key: "Honiara", offsetMinutes: 660 } ] },
    { x: 0.9855, y: 0.7047, choices: [ { key: "Auckland", offsetMinutes: 720 }, { key: "Wellington", offsetMinutes: 720 }, { key: "Chatham", offsetMinutes: 765 }, { key: "Nuku'alofa", offsetMinutes: 780 }, { key: "Apia", offsetMinutes: 780 }, { key: "Kiritimati", offsetMinutes: 840 } ] }
]
