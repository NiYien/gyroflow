// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

function filenameFromUrl(url) {
    const text = url.toString().toLowerCase();
    const trimmed = text.replace(/[\\/]+$/, "");
    return trimmed.split(/[\\/]/).pop();
}

function acceptsUrl(url, extensions, acceptedFilenameSuffixes) {
    const filename = filenameFromUrl(url);
    if (!filename) return true;

    // Native filesystem check: a real directory is accepted (even if its
    // name contains dots like `Footage.2024`) ONLY when this drop target
    // opts in to folders via acceptedFilenameSuffixes (the main drop area
    // and render queue both list ".rdc"/".rdm"/"_mix.bin"). Single-file
    // targets (lens profile, motion data, video info) keep the old
    // file-only behavior.
    const acceptsFolders = (acceptedFilenameSuffixes || []).length > 0;
    if (acceptsFolders && typeof filesystem !== "undefined" && filesystem.is_dir(url)) return true;

    for (const suffix of acceptedFilenameSuffixes || []) {
        if (filename.endsWith(suffix.toLowerCase())) return true;
    }

    const dot = filename.lastIndexOf(".");
    if (dot < 0) return true;

    const ext = filename.substring(dot + 1);
    for (const accepted of extensions || []) {
        if (ext === accepted.toString().replace(/^\./, "").toLowerCase()) return true;
    }
    return false;
}

function acceptsAnyUrl(urls, extensions, acceptedFilenameSuffixes) {
    for (const url of urls || []) {
        if (acceptsUrl(url, extensions, acceptedFilenameSuffixes)) return true;
    }
    return false;
}
