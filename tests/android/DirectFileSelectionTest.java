// SPDX-License-Identifier: GPL-3.0-or-later
package com.niyien.gyroflow;

import java.io.File;
import java.util.Arrays;
import java.util.Collections;

public final class DirectFileSelectionTest {
    private static void check(boolean value, String message) {
        if (!value) throw new AssertionError(message);
    }
    public static void main(String[] args) {
        DirectFileSelection video = new DirectFileSelection(Arrays.asList(".mp4", ".mov", ".gyroflow"));
        check(video.accepts("中文素材.MP4"), "case-insensitive video extension");
        check(video.accepts("project.gyroflow"), "project files");
        check(!video.accepts("movie.mp4.txt"), "extension must match the end");
        check(!video.accepts("record_mix.bin"), "video filter excludes gyro");
        File first = new File("storage/A/片段.MP4"), second = new File("usb/B/片段.MP4");
        video.toggle(first); video.toggle(second);
        check(video.size() == 2 && video.contains(first) && video.contains(second), "selection survives directories and volumes");
        check(video.snapshot().equals(Arrays.asList(first, second)), "result preserves selected paths and order");
        video.snapshot().clear();
        check(video.size() == 2, "result is a copy");
        video.toggle(first);
        check(video.size() == 1 && !video.contains(first), "unselect a file");
        video.toggle(new File("notes.txt"));
        check(video.size() == 1, "unsupported files cannot be selected");
        DirectFileSelection gyro = new DirectFileSelection(Collections.singletonList("_mix.bin"));
        check(gyro.accepts("DATA_MIX.BIN") && !gyro.accepts("other.bin"), "gyro suffix filter");
        check(!new DirectFileSelection(Collections.emptyList()).accepts("a.mp4"), "missing filter never admits every file");
        System.out.println("PASS: 11 direct file selection cases");
    }
}
