package com.niyien.gyroflow;

public final class StorageVolumeLabelTest {
    private static void check(String expected, String input) {
        String actual = StorageVolumeLabel.displayName(input);
        if (!java.util.Objects.equals(expected, actual))
            throw new AssertionError("Expected " + expected + ", got " + actual);
    }

    public static void main(String[] args) {
        check("未命名", "M-fM-^\\M-*M-eM-^QM-=M-eM-^PM-^M");
        check("Backup 未命名 2026", "Backup M-fM-^\\M-*M-eM-^QM-=M-eM-^PM-^M 2026");
        check("内置 未命名", "内置 M-fM-^\\M-*M-eM-^QM-=M-eM-^PM-^M");
        check("内部存储设备", "内部存储设备");
        check("Samsung SSD", "Samsung SSD");
        check("M-Backup", "M-Backup");
        check("M-fM-^", "M-fM-^");
        check("M-fM-^\\", "M-fM-^\\");
        check("M-^@", "M-^@");
        check("M-BM-^E", "M-BM-^E");
        check("M-?M-?", "M-?M-?");
        check("", "");
        check(null, null);
        System.out.println("PASS: 13 volume label cases");
    }
}
