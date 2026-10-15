// SPDX-License-Identifier: GPL-3.0-or-later
package com.niyien.stabilizer;

import android.graphics.Bitmap;
import android.graphics.drawable.Drawable;
import android.media.ThumbnailUtils;
import android.os.CancellationSignal;
import android.util.LruCache;
import android.util.Size;
import android.widget.TextView;
import java.io.File;
import java.util.HashMap;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.Future;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;

/** Loads only bound rows, independently of directory enumeration. */
final class VideoThumbnailLoader {
    private final ThreadPoolExecutor workers = new ThreadPoolExecutor(2, 2, 0,
            TimeUnit.MILLISECONDS, new LinkedBlockingQueue<>());
    private final Map<TextView, Request> rows = new HashMap<>();
    private final LruCache<String, Bitmap> cache = new LruCache<String, Bitmap>(8 * 1024 * 1024) {
        @Override protected int sizeOf(String key, Bitmap image) { return image.getAllocationByteCount(); }
    };
    private final LruCache<String, Boolean> failures = new LruCache<>(128);
    private boolean closed;

    private static final class Request {
        final String path;
        final CancellationSignal cancellation = new CancellationSignal();
        volatile boolean active = true;
        Future<?> task;
        Request(File file) { path = file.getAbsolutePath(); }
        void cancel() {
            active = false;
            cancellation.cancel();
            if (task != null) task.cancel(true);
        }
    }

    static boolean supports(String name) {
        String lower = name.toLowerCase(Locale.ROOT);
        return lower.endsWith(".mp4") || lower.endsWith(".mov") || lower.endsWith(".m4v")
                || lower.endsWith(".mkv") || lower.endsWith(".webm") || lower.endsWith(".avi")
                || lower.endsWith(".mts") || lower.endsWith(".m2ts") || lower.endsWith(".3gp");
    }

    void unbind(TextView view) {
        Request old = rows.remove(view);
        if (old != null) old.cancel();
    }

    void bind(TextView view, File file) {
        if (closed) return;
        Request old = rows.get(view);
        if (old != null && old.active && old.path.equals(file.getAbsolutePath())) return;
        unbind(view);
        int width = Math.round(72 * view.getResources().getDisplayMetrics().density);
        int height = Math.round(44 * view.getResources().getDisplayMetrics().density);
        Drawable placeholder = view.getContext().getDrawable(android.R.drawable.ic_media_play).mutate();
        placeholder.setTint(view.getCurrentTextColor());
        placeholder.setBounds(0, 0, width, height);
        view.setCompoundDrawablesRelative(placeholder, null, null, null);
        Request request = new Request(file);
        rows.put(view, request);
        workers.purge();
        request.task = workers.submit(() -> {
            if (!request.active) return;
            // Stat and decode stay off the UI thread, including on removable storage.
            String key = request.path + "\n" + file.length() + "\n" + file.lastModified();
            Bitmap bitmap = cache.get(key);
            try {
                if (bitmap == null && failures.get(key) == null) {
                    bitmap = ThumbnailUtils.createVideoThumbnail(file, new Size(256, 160), request.cancellation);
                    if (request.active) cache.put(key, bitmap);
                }
            } catch (Exception error) {
                if (request.active) failures.put(key, true);
            }
            final Bitmap result = bitmap;
            if (!request.active || result == null) return;
            view.post(() -> {
                if (closed || !request.active || rows.get(view) != request) return;
                Drawable drawable = new Drawable() {
                    final android.graphics.Paint paint = new android.graphics.Paint(android.graphics.Paint.FILTER_BITMAP_FLAG);
                    @Override public void draw(android.graphics.Canvas canvas) {
                        android.graphics.Rect bounds = getBounds();
                        float scale = Math.min(bounds.width() / (float) result.getWidth(), bounds.height() / (float) result.getHeight());
                        float w = result.getWidth() * scale, h = result.getHeight() * scale;
                        float x = bounds.left + (bounds.width() - w) / 2, y = bounds.top + (bounds.height() - h) / 2;
                        canvas.drawBitmap(result, null, new android.graphics.RectF(x, y, x + w, y + h), paint);
                    }
                    @Override public void setAlpha(int alpha) { paint.setAlpha(alpha); }
                    @Override public void setColorFilter(android.graphics.ColorFilter filter) { paint.setColorFilter(filter); }
                    @Override public int getOpacity() { return android.graphics.PixelFormat.TRANSLUCENT; }
                };
                drawable.setBounds(0, 0, width, height);
                view.setCompoundDrawablesRelative(drawable, null, null, null);
            });
        });
    }

    void cancelRows() {
        for (Request request : rows.values()) request.cancel();
        rows.clear();
        workers.purge();
    }

    void close() {
        closed = true;
        cancelRows();
        workers.shutdownNow();
        cache.evictAll();
        failures.evictAll();
    }
}
