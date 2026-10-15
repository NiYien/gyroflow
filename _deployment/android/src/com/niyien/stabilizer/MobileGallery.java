// SPDX-License-Identifier: GPL-3.0-or-later
package com.niyien.stabilizer;

import android.content.ContentValues;
import android.content.Context;
import android.media.MediaScannerConnection;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.MediaStore;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.OutputStream;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/** Called from a render worker after the video file has been closed. */
final class MobileGallery {
    static String save(Context context, String path) {
        Uri created = null;
        File legacyFile = null;
        try {
            File source = new File(path);
            if (!source.isFile() || source.length() == 0) return "error:missing_file";
            String extensionName = source.getName().toLowerCase(java.util.Locale.ROOT);
            if (!extensionName.endsWith(".mp4") && !extensionName.endsWith(".mov") && !extensionName.endsWith(".m4v"))
                return "error:unsupported_format";
            String mime = source.getName().toLowerCase(java.util.Locale.ROOT).endsWith(".mov")
                    ? "video/quicktime" : "video/mp4";
            if (Build.VERSION.SDK_INT >= 29) {
                ContentValues values = new ContentValues();
                values.put(MediaStore.Video.Media.DISPLAY_NAME, source.getName());
                values.put(MediaStore.Video.Media.MIME_TYPE, mime);
                values.put(MediaStore.Video.Media.RELATIVE_PATH, Environment.DIRECTORY_MOVIES + "/NiYien");
                values.put(MediaStore.Video.Media.IS_PENDING, 1);
                created = context.getContentResolver().insert(
                        MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY), values);
                if (created == null) throw new java.io.IOException("Unable to create media entry");
                try (OutputStream output = context.getContentResolver().openOutputStream(created, "w")) {
                    if (output == null) throw new java.io.IOException("Unable to open media entry");
                    copy(source, output);
                }
                values.clear();
                values.put(MediaStore.Video.Media.IS_PENDING, 0);
                if (context.getContentResolver().update(created, values, null, null) != 1)
                    throw new java.io.IOException("Unable to publish media entry");
                return "ok:" + created;
            }
            File folder = new File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_MOVIES), "NiYien");
            if (!folder.isDirectory() && !folder.mkdirs()) throw new java.io.IOException("Unable to create Movies/NiYien");
            String name = source.getName();
            int dot = name.lastIndexOf('.');
            String base = dot < 0 ? name : name.substring(0, dot);
            String extension = dot < 0 ? ".mp4" : name.substring(dot);
            legacyFile = new File(folder, name);
            for (int n = 1; !legacyFile.createNewFile(); n++) legacyFile = new File(folder, base + "-" + n + extension);
            try (OutputStream output = new FileOutputStream(legacyFile)) { copy(source, output); }
            final Uri[] scanned = new Uri[1];
            CountDownLatch done = new CountDownLatch(1);
            MediaScannerConnection.scanFile(context, new String[] { legacyFile.getAbsolutePath() },
                    new String[] { mime }, (file, uri) -> { scanned[0] = uri; done.countDown(); });
            if (!done.await(60, TimeUnit.SECONDS) || scanned[0] == null)
                throw new java.io.IOException("Media scan failed");
            return "ok:" + scanned[0];
        } catch (Exception error) {
            android.util.Log.w("NiYienGallery", "Saving video failed", error);
            if (created != null) {
                try { context.getContentResolver().delete(created, null, null); } catch (Exception ignored) { }
            }
            if (legacyFile != null) legacyFile.delete();
            return "error:" + (error instanceof SecurityException ? "permission_denied" : "save_failed");
        }
    }

    private static void copy(File source, OutputStream output) throws java.io.IOException {
        try (FileInputStream input = new FileInputStream(source)) {
            byte[] buffer = new byte[1024 * 1024];
            int count;
            while ((count = input.read(buffer)) != -1) output.write(buffer, 0, count);
            output.flush();
        }
    }
}
