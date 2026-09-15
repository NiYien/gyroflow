// SPDX-License-Identifier: GPL-3.0-or-later
package com.niyien.stabilizer;

import java.io.File;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;

/** File selection survives navigation without importing folders implicitly. */
final class DirectFileSelection {
    private final List<String> suffixes;
    private final LinkedHashSet<File> files = new LinkedHashSet<>();

    DirectFileSelection(List<String> suffixes) { this.suffixes = new ArrayList<>(suffixes); }

    boolean accepts(String name) {
        String lower = name.toLowerCase(Locale.ROOT);
        for (String suffix : suffixes) {
            if (!suffix.isEmpty() && lower.endsWith(suffix.toLowerCase(Locale.ROOT))) return true;
        }
        return false;
    }
    void toggle(File file) { if (!files.remove(file) && accepts(file.getName())) files.add(file); }
    boolean contains(File file) { return files.contains(file); }
    int size() { return files.size(); }
    List<File> snapshot() { return new ArrayList<>(files); }
}
