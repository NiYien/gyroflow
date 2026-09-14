// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
.pragma library

function selectionRange(rows, previous, from, to, selected) {
    const result = Object.assign({}, previous);
    for (let i = Math.max(0, Math.min(from, to)); i <= Math.min(rows.length - 1, Math.max(from, to)); ++i) {
        if (selected) result[rows[i].id] = true;
        else delete result[rows[i].id];
    }
    return result;
}
function pruneSelection(rows, selection) {
    const result = {};
    rows.forEach(row => { if (selection[row.id]) result[row.id] = true; });
    return result;
}
function findRow(rows, id) {
    return rows.find(row => row.id === id) || null;
}
function duration(ms) {
    const seconds = Math.max(0, Math.floor(ms / 1000));
    return Math.floor(seconds / 60).toString().padStart(2, "0") + ":" + (seconds % 60).toString().padStart(2, "0");
}
function parse(value, fallback) {
    try { return JSON.parse(value); } catch (e) { return fallback; }
}
function previewSnapshot(value) {
    const snapshot = Object.assign({}, value);
    // Reference-only projects still need their file URL for read-only import.
    if (snapshot.videofile && snapshot.stabilization) delete snapshot.project_file;
    return snapshot;
}
function beginOperation(id, kind, rows) {
    const baseline = {};
    rows.forEach(row => { baseline[row.id] = { status: row.status, epoch: row.epoch || 0 }; });
    return { id: id, kind: kind, active: true, started: false, stopping: false,
        targets: rows.map(row => row.id), baseline: baseline, touched: {}, results: {}, details: {} };
}
function touchOperation(operation, id) {
    if (!operation || !operation.active || operation.targets.indexOf(id) < 0) return operation;
    return Object.assign({}, operation, { started: true, touched: Object.assign({}, operation.touched, { [id]: true }) });
}
function observeOperation(operation, rows, final) {
    if (!operation) return null;
    const touched = Object.assign({}, operation.touched);
    const results = Object.assign({}, operation.results);
    const details = Object.assign({}, operation.details);
    operation.targets.forEach(id => {
        const row = findRow(rows, id);
        if (!row) { if (final) results[id] = "skipped"; return; }
        const baseline = operation.baseline[id];
        if (row.status === "Rendering" || (row.epoch || 0) !== baseline.epoch) touched[id] = true;
        const completedStage = operation.kind === "export" ? row.lastExport === 4 || row.lastExport === 0 : row.lastExport === 2;
        if (row.status === "Finished" && completedStage && (touched[id] || baseline.status !== "Finished")) results[id] = "success";
        else if (row.status === "Error" && !/^(file_exists:|convert_format:)/.test(row.error || "")) results[id] = "failed";
        else if (row.status === "Skipped") results[id] = "skipped";
        else if (final && !results[id]) results[id] = operation.stopping ? "cancelled" : "skipped";
        if (results[id]) details[id] = Object.assign({}, row);
    });
    return Object.assign({}, operation, { touched: touched, results: results, details: details, active: !final });
}
function counts(operation) {
    const counts = { success: 0, failed: 0, skipped: 0, cancelled: 0, settled: 0, total: 0 };
    if (!operation) return counts;
    counts.total = operation.targets.length;
    Object.keys(operation.results).forEach(id => {
        counts[operation.results[id]]++;
        counts.settled++;
    });
    return counts;
}
