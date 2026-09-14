// SPDX-License-Identifier: GPL-3.0-or-later
package com.niyien.gyroflow;

import android.app.Dialog;
import android.content.Context;
import android.graphics.Color;
import android.graphics.Typeface;
import android.os.Bundle;
import android.os.Environment;
import android.os.storage.StorageManager;
import android.os.storage.StorageVolume;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowInsets;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.CheckedTextView;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.Spinner;
import android.widget.TextView;
import org.json.JSONObject;
import java.io.File;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.function.Consumer;

/** A folder browser whose Back action always cancels the whole selection. */
final class DirectFolderDialog extends Dialog {
    private final JSONObject labels;
    private final Consumer<List<File>> accepted;
    private final boolean filesMode;
    private final DirectFileSelection selection;
    private final Runnable cancelled;
    private final ExecutorService reader = Executors.newSingleThreadExecutor();
    private final List<File> roots = new ArrayList<>();
    private final List<String> volumeNames = new ArrayList<>();
    private File current;
    private File volumeRoot;
    private File[] entries = new File[0];
    private TextView path, status;
    private Button up, select;
    private ArrayAdapter<String> adapter;
    private int request;
    private boolean completed;
    private final boolean dark;
    private final int foreground;

    DirectFolderDialog(Context context, String options, File initial, Consumer<List<File>> accepted, Runnable cancelled) {
        super(context, parse(options).optBoolean("dark") ? android.R.style.Theme_DeviceDefault_NoActionBar : android.R.style.Theme_DeviceDefault_Light_NoActionBar);
        labels = parse(options);
        dark = labels.optBoolean("dark");
        filesMode = labels.optBoolean("files");
        List<String> suffixes = new ArrayList<>();
        org.json.JSONArray allowed = labels.optJSONArray("suffixes");
        if (allowed != null) for (int i = 0; i < allowed.length(); ++i) suffixes.add(allowed.optString(i));
        selection = new DirectFileSelection(suffixes);
        foreground = dark ? 0xffededee : 0xff202329;
        this.accepted = accepted;
        this.cancelled = cancelled;
        current = initial;
        setOnCancelListener(dialog -> {
            if (!completed) { completed = true; cancelled.run(); }
        });
        setOnDismissListener(dialog -> { ++request; reader.shutdownNow(); });
    }

    private static JSONObject parse(String value) {
        try { return new JSONObject(value); } catch (Exception ignored) { return new JSONObject(); }
    }
    private String label(String key, String fallback) { return labels.optString(key, fallback); }
    private int dp(int value) { return Math.round(value * getContext().getResources().getDisplayMetrics().density); }
    private TextView text(String value, int size) {
        TextView view = new TextView(getContext());
        view.setText(value); view.setTextSize(size); view.setTextColor(foreground);
        view.setGravity(Gravity.CENTER_VERTICAL);
        return view;
    }
    private Button button(String value) {
        Button button = new Button(getContext());
        button.setText(value); button.setAllCaps(false); button.setTextColor(foreground);
        button.setTextSize(16);
        button.setBackground(new android.graphics.drawable.RippleDrawable(
                android.content.res.ColorStateList.valueOf(0x222859bf), null, new android.graphics.drawable.ColorDrawable(Color.WHITE)));
        button.setMinHeight(dp(48));
        return button;
    }
    private LinearLayout row() {
        LinearLayout row = new LinearLayout(getContext());
        row.setGravity(Gravity.CENTER_VERTICAL);
        return row;
    }

    @Override protected void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout content = new LinearLayout(getContext());
        content.setOrientation(LinearLayout.VERTICAL);
        content.setBackgroundColor(dark ? 0xff16191f : 0xfff4f5f7);
        content.setPadding(dp(16), dp(8), dp(16), dp(16));
        setContentView(content);
        Window window = getWindow();
        window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT);
        window.setDecorFitsSystemWindows(false);
        content.setOnApplyWindowInsetsListener((view, insets) -> {
            android.graphics.Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            view.setPadding(bars.left + dp(16), bars.top + dp(8), bars.right + dp(16), bars.bottom + dp(16));
            return WindowInsets.CONSUMED;
        });
        content.requestApplyInsets();

        LinearLayout header = row();
        TextView title = text(label("title", "Choose folders"), 20);
        title.setTypeface(null, Typeface.BOLD);
        header.addView(title, new LinearLayout.LayoutParams(0, dp(56), 1));
        Button cancel = button(label("cancel", "Cancel"));
        cancel.setTextColor(dark ? 0xff8bb3ff : 0xff2859bf);
        cancel.setOnClickListener(view -> cancel());
        header.addView(cancel);
        content.addView(header);

        StorageManager manager = getContext().getSystemService(StorageManager.class);
        for (StorageVolume volume : manager.getStorageVolumes()) {
            File root = volume.getDirectory();
            if (root != null && root.isDirectory()) {
                roots.add(root); volumeNames.add(StorageVolumeLabel.displayName(volume.getDescription(getContext())));
            }
        }
        if (roots.isEmpty()) {
            roots.add(Environment.getExternalStorageDirectory());
            volumeNames.add(roots.get(0).getName());
        }
        int selectedVolume = 0;
        for (int i = 0; i < roots.size(); ++i) {
            if (inside(current, roots.get(i))) { selectedVolume = i; break; }
        }
        volumeRoot = roots.get(selectedVolume);
        if (!inside(current, volumeRoot) || !current.isDirectory()) current = volumeRoot;
        if (roots.size() > 1) {
            Spinner volumes = new Spinner(getContext());
            ArrayAdapter<String> volumesAdapter = new ArrayAdapter<String>(getContext(), android.R.layout.simple_spinner_item, volumeNames) {
                private View volumeView(int position, boolean dropdown) {
                    TextView view = text(getItem(position), 16);
                    view.setSingleLine(true);
                    view.setEllipsize(android.text.TextUtils.TruncateAt.END);
                    view.setPadding(dp(12), 0, dp(12), 0);
                    view.setMinHeight(dp(48));
                    view.setBackgroundColor(dropdown ? (dark ? 0xff202329 : Color.WHITE) : Color.TRANSPARENT);
                    return view;
                }
                @Override public View getView(int position, View convert, ViewGroup parent) { return volumeView(position, false); }
                @Override public View getDropDownView(int position, View convert, ViewGroup parent) { return volumeView(position, true); }
            };
            volumes.setPopupBackgroundDrawable(new android.graphics.drawable.ColorDrawable(dark ? 0xff202329 : Color.WHITE));
            volumes.setAdapter(volumesAdapter); volumes.setSelection(selectedVolume);
            volumes.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
                @Override public void onItemSelected(AdapterView<?> parent, View view, int position, long id) {
                    if (!volumeRoot.equals(roots.get(position))) { volumeRoot = roots.get(position); browse(volumeRoot); }
                }
                @Override public void onNothingSelected(AdapterView<?> parent) {}
            });
            content.addView(volumes, new LinearLayout.LayoutParams(-1, dp(48)));
        }
        path = text("", 14); path.setMaxLines(2);
        path.setEllipsize(android.text.TextUtils.TruncateAt.START);
        content.addView(path, new LinearLayout.LayoutParams(-1, dp(48)));
        up = button("↑  " + label("up", "Up one level"));
        up.setGravity(Gravity.START | Gravity.CENTER_VERTICAL);
        up.setOnClickListener(view -> browse(current.getParentFile()));
        content.addView(up, new LinearLayout.LayoutParams(-1, dp(48)));

        ListView list = new ListView(getContext());
        list.setDividerHeight(dp(1));
        adapter = new ArrayAdapter<String>(getContext(), filesMode ? android.R.layout.simple_list_item_multiple_choice : android.R.layout.simple_list_item_1, new ArrayList<>()) {
            @Override public View getView(int position, View convert, ViewGroup parent) {
                TextView view = (TextView) super.getView(position, convert, parent);
                view.setTextColor(foreground); view.setTextSize(16); view.setMinHeight(dp(56));
                view.setSingleLine(true); view.setEllipsize(android.text.TextUtils.TruncateAt.MIDDLE);
                boolean file = filesMode && position < entries.length && !entries[position].isDirectory();
                if (view instanceof CheckedTextView) {
                    CheckedTextView checked = (CheckedTextView) view;
                    android.util.TypedValue indicator = new android.util.TypedValue();
                    getContext().getTheme().resolveAttribute(android.R.attr.listChoiceIndicatorMultiple, indicator, true);
                    checked.setCheckMarkDrawable(file ? indicator.resourceId : 0);
                    checked.setChecked(file && selection.contains(entries[position]));
                }
                android.graphics.drawable.Drawable icon = new android.graphics.drawable.Drawable() {
                    final android.graphics.Paint paint = new android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG);
                    @Override public void draw(android.graphics.Canvas canvas) {
                        canvas.save(); canvas.translate(getBounds().left, getBounds().top); canvas.scale(dp(24) / 24f, dp(24) / 24f);
                        paint.setColor(foreground); paint.setStyle(android.graphics.Paint.Style.STROKE); paint.setStrokeWidth(1.5f);
                        android.graphics.Path shape = new android.graphics.Path();
                        shape.moveTo(3, 6); shape.lineTo(10, 6); shape.lineTo(12, 8); shape.lineTo(21, 8);
                        shape.lineTo(21, 20); shape.lineTo(3, 20); shape.close(); canvas.drawPath(shape, paint); canvas.restore();
                    }
                    @Override public void setAlpha(int alpha) {}
                    @Override public void setColorFilter(android.graphics.ColorFilter filter) {}
                    @Override public int getOpacity() { return android.graphics.PixelFormat.TRANSLUCENT; }
                };
                icon.setBounds(0, 0, dp(24), dp(24));
                view.setCompoundDrawablesRelative(file ? null : icon, null, null, null);
                view.setCompoundDrawablePadding(dp(12));
                return view;
            }
        };
        list.setAdapter(adapter);
        list.setOnItemClickListener((parent, view, position, id) -> {
            if (position >= entries.length) return;
            File entry = entries[position];
            if (entry.isDirectory()) browse(entry);
            else if (filesMode) {
                selection.toggle(entry);
                adapter.notifyDataSetChanged();
                updateSelection();
            }
        });
        content.addView(list, new LinearLayout.LayoutParams(-1, 0, 1));
        status = text("", 14);
        status.setPadding(0, dp(8), 0, dp(8));
        content.addView(status);
        select = button(label("select", "Add this folder"));
        android.graphics.drawable.GradientDrawable fill = new android.graphics.drawable.GradientDrawable();
        fill.setColor(dark ? 0xff407de0 : 0xff2859bf); fill.setCornerRadius(dp(4));
        select.setBackground(new android.graphics.drawable.RippleDrawable(android.content.res.ColorStateList.valueOf(0x33ffffff), fill, null));
        select.setTextColor(Color.WHITE);
        select.setOnClickListener(view -> {
            List<File> result = new ArrayList<>();
            if (filesMode) {
                result = selection.snapshot();
                if (result.isEmpty()) return;
                for (File file : result) {
                    if (!file.isFile() || !file.canRead()) {
                        selection.toggle(file);
                        adapter.notifyDataSetChanged();
                        updateSelection();
                        status.setText(label("error", "Unable to read this folder"));
                        status.setVisibility(View.VISIBLE);
                        return;
                    }
                }
            } else {
                if (!current.isDirectory() || !current.canRead()) { browse(current); return; }
                result.add(current);
            }
            completed = true;
            dismiss();
            accepted.accept(result);
        });
        content.addView(select, new LinearLayout.LayoutParams(-1, dp(52)));
        browse(current);
    }

    private void updateSelection() {
        if (!filesMode) return;
        select.setText(label("select", "Add %1 files").replace("%1", Integer.toString(selection.size())));
        select.setEnabled(selection.size() > 0);
    }

    private boolean inside(File file, File root) {
        if (file == null) return false;
        try {
            String child = file.getCanonicalPath(), base = root.getCanonicalPath();
            return child.equals(base) || child.startsWith(base + File.separator);
        } catch (java.io.IOException ignored) { return false; }
    }
    private void browse(File folder) {
        if (!inside(folder, volumeRoot)) return;
        current = folder;
        path.setText(folder.getAbsolutePath());
        up.setEnabled(!folder.equals(volumeRoot));
        select.setEnabled(false);
        updateSelection();
        entries = new File[0]; adapter.clear();
        status.setVisibility(View.VISIBLE); status.setText(label("loading", "Reading…"));
        final int ticket = ++request;
        reader.execute(() -> {
            File[] children;
            try { children = folder.listFiles(file -> file.isDirectory() || (filesMode && file.isFile() && selection.accepts(file.getName()))); }
            catch (SecurityException ignored) { children = null; }
            if (children != null) Arrays.sort(children, (a, b) -> a.isDirectory() == b.isDirectory()
                    ? a.getName().compareToIgnoreCase(b.getName()) : a.isDirectory() ? -1 : 1);
            final File[] result = children;
            path.post(() -> {
                if (completed || ticket != request) return;
                entries = result == null ? new File[0] : result;
                List<String> names = new ArrayList<>();
                for (File child : entries) names.add(child.getName());
                adapter.addAll(names);
                status.setText(result == null ? label("error", "Unable to read this folder") : label("empty", "No subfolders"));
                status.setVisibility(result == null || result.length == 0 ? View.VISIBLE : View.GONE);
                if (filesMode) updateSelection(); else select.setEnabled(result != null);
            });
        });
    }
}
