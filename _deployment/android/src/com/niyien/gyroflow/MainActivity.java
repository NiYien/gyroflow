package com.niyien.gyroflow;

import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.hardware.usb.UsbConstants;
import android.hardware.usb.UsbDevice;
import android.hardware.usb.UsbDeviceConnection;
import android.hardware.usb.UsbEndpoint;
import android.hardware.usb.UsbInterface;
import android.hardware.usb.UsbManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;

import java.util.Arrays;
import java.util.Map;

public class MainActivity extends org.qtproject.qt.android.bindings.QtActivity {
    private static final String TAG = "GyroflowNiYienUsb";
    private static final String ACTION_USB_PERMISSION = "com.niyien.gyroflow.USB_PERMISSION";
    private static final int NIYIEN_VENDOR_ID = 0xffff;
    private static final int NIYIEN_PRODUCT_ID = 0xffff;
    private static final int READ_TIMEOUT_MS = 50;
    private static final int WRITE_TIMEOUT_MS = 1000;
    private static final int CONTROL_TIMEOUT_MS = 1000;
    private static final int USB_PERMISSION_REQUEST_TIMEOUT_MS = 5000;
    private static final int CDC_SET_LINE_CODING = 0x20;
    private static final int CDC_SET_CONTROL_LINE_STATE = 0x22;
    private static final int CDC_REQUEST_TYPE_OUT = 0x21;
    private static final int CDC_CONTROL_LINE_DTR_RTS = 0x03;
    private static final int CDC_BAUD_RATE = 2_000_000;

    // Workaround for Qt 6.7.3 SAF result-parsing bug on Android 16 / MIUI:
    // MIUI hijacks ACTION_OPEN_DOCUMENT to its private fileexplorer using
    // hyper.intent.action.OPEN_DOCUMENT, and Qt's internal FileDialog parser
    // returns empty selectedFile/selectedFiles. We additionally observe the
    // raw Intent in onActivityResult and forward the URI through the existing
    // urlReceived JNI bridge (already used for VIEW/SEND intents).
    private static final String FILE_PICKER_TAG = "GyroflowNiYienPicker";
    private static final long PICKER_DEDUPE_WINDOW_MS = 1500;

    private static MainActivity instance;

    private UsbManager usbManager;
    private PendingIntent usbPermissionIntent;
    private UsbDevice currentDevice;
    private UsbDeviceConnection usbConnection;
    private UsbInterface usbControlInterface;
    private UsbInterface usbInterface;
    private UsbEndpoint usbInEndpoint;
    private UsbEndpoint usbOutEndpoint;
    private UsbDevice pendingPermissionDevice;
    private UsbDevice deniedPermissionDevice;
    private boolean usbPermissionRequestInFlight;
    private long usbPermissionRequestedAtMs;
    private Thread usbReadThread;
    private volatile boolean usbReadRunning;
    private final Object usbLock = new Object();

    private String lastDispatchedPickerUri;
    private long lastDispatchedPickerAtMs;

    public static native void urlReceived(String url);
    public static native void nativeOnUsbAttached(int vid, int pid);
    public static native void nativeOnUsbDetached();
    public static native void nativeOnUsbPermission(boolean granted);
    public static native void nativeOnUsbOpened(int vid, int pid);
    public static native void nativeOnUsbBytes(byte[] buf, int len);
    public static native void nativeOnUsbWriteResult(boolean ok, String err);
    public static native void nativeOnUsbError(String err);

    private final BroadcastReceiver usbReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            String action = intent.getAction();
            UsbDevice device = getUsbDevice(intent);
            if (ACTION_USB_PERMISSION.equals(action)) {
                boolean granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false);
                Log.i(TAG, "USB permission result granted=" + granted + " device=" + describeDevice(device));
                clearPendingPermissionRequest(device);
                if (granted) {
                    clearDeniedPermission(device);
                } else {
                    markDeniedPermission(device);
                }
                nativeOnUsbPermission(granted);
                if (granted && isNiyienDevice(device)) {
                    openUsbDevice(device);
                }
                return;
            }
            if (UsbManager.ACTION_USB_DEVICE_ATTACHED.equals(action)) {
                Log.i(TAG, "USB device attached " + describeDevice(device));
                handleUsbDeviceAttached(device);
                return;
            }
            if (UsbManager.ACTION_USB_DEVICE_DETACHED.equals(action)) {
                Log.i(TAG, "USB device detached " + describeDevice(device));
                if (isCurrentDevice(device) || isNiyienDevice(device)) {
                    clearPendingPermissionRequest(device);
                    clearDeniedPermission(device);
                    closeUsbDevice();
                    nativeOnUsbDetached();
                }
            }
        }
    };

    @Override
    public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        instance = this;
        initUsbBridge();
        Intent intent = getIntent();
        if (intent != null && intent.getAction() != null) {
            processIntent(intent);
        }
    }

    @Override
    public void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        processIntent(intent);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        // Always forward to QtActivity first so Qt's own FileDialog state
        // machine completes its result-listener cleanup. Our parsing below
        // is purely additive: when Qt's parser fails (Android 16 + MIUI
        // fileexplorer scenario), we still surface the picked URI.
        super.onActivityResult(requestCode, resultCode, data);

        if (resultCode != RESULT_OK || data == null) {
            return;
        }

        Uri picked = extractPickerUri(data);
        if (picked == null) {
            Log.w(FILE_PICKER_TAG,
                "onActivityResult RESULT_OK but no Uri in data/clipData"
                    + " requestCode=" + requestCode
                    + " action=" + data.getAction()
                    + " type=" + data.getType());
            return;
        }

        String uriStr = picked.toString();
        long now = System.currentTimeMillis();
        synchronized (this) {
            if (uriStr.equals(lastDispatchedPickerUri)
                    && now - lastDispatchedPickerAtMs < PICKER_DEDUPE_WINDOW_MS) {
                Log.i(FILE_PICKER_TAG, "skip duplicate picker dispatch " + uriStr);
                return;
            }
            lastDispatchedPickerUri = uriStr;
            lastDispatchedPickerAtMs = now;
        }

        Log.i(FILE_PICKER_TAG,
            "dispatching picker uri to Rust: " + uriStr
                + " requestCode=" + requestCode
                + " manufacturer=" + Build.MANUFACTURER);
        urlReceived(uriStr);
    }

    @Override
    protected void onDestroy() {
        closeUsbDevice();
        try {
            unregisterReceiver(usbReceiver);
        } catch (IllegalArgumentException ignored) {
        }
        if (instance == this) {
            instance = null;
        }
        super.onDestroy();
    }

    public static void requestUsbDeviceScan() {
        MainActivity activity = instance;
        if (activity == null) {
            nativeOnUsbError("Android activity is not ready for USB scan");
            return;
        }
        activity.runOnUiThread(activity::scanForNiyienDevices);
    }

    public static boolean writeDeviceBytes(byte[] data) {
        MainActivity activity = instance;
        if (activity == null) {
            nativeOnUsbWriteResult(false, "Android activity is not ready for USB write");
            return false;
        }
        return activity.writeUsbDeviceBytes(data);
    }

    public static void closeDeviceFromRust() {
        MainActivity activity = instance;
        if (activity != null) {
            activity.closeUsbDevice();
        }
    }

    private void initUsbBridge() {
        usbManager = (UsbManager)getSystemService(Context.USB_SERVICE);
        if (usbManager == null) {
            nativeOnUsbError("Android UsbManager is unavailable");
            return;
        }

        int flags = PendingIntent.FLAG_UPDATE_CURRENT;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            flags |= PendingIntent.FLAG_MUTABLE;
        }
        Intent permissionIntent = new Intent(ACTION_USB_PERMISSION).setPackage(getPackageName());
        usbPermissionIntent = PendingIntent.getBroadcast(this, 0, permissionIntent, flags);

        IntentFilter filter = new IntentFilter();
        filter.addAction(ACTION_USB_PERMISSION);
        filter.addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED);
        filter.addAction(UsbManager.ACTION_USB_DEVICE_DETACHED);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(usbReceiver, filter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(usbReceiver, filter);
        }

        scanForNiyienDevices();
    }

    private void processIntent(Intent intent) {
        if (UsbManager.ACTION_USB_DEVICE_ATTACHED.equals(intent.getAction())) {
            handleUsbDeviceAttached(getUsbDevice(intent));
            return;
        }

        Uri uri;
        if ("android.intent.action.VIEW".equals(intent.getAction())) {
            uri = intent.getData();
        } else if ("android.intent.action.SEND".equals(intent.getAction())) {
            uri = (Uri)intent.getExtras().get(Intent.EXTRA_STREAM);
        } else {
            return;
        }
        if (uri != null) {
            urlReceived(uri.toString());
        }
    }

    private void scanForNiyienDevices() {
        if (usbManager == null) {
            nativeOnUsbError("Android UsbManager is unavailable");
            return;
        }
        Map<String, UsbDevice> devices = usbManager.getDeviceList();
        for (UsbDevice device : devices.values()) {
            if (isNiyienDevice(device)) {
                handleUsbDeviceAttached(device);
                return;
            }
        }
    }

    private void handleUsbDeviceAttached(UsbDevice device) {
        if (!isNiyienDevice(device)) {
            return;
        }

        if (isCurrentDeviceOpen(device)) {
            return;
        }

        if (usbManager.hasPermission(device)) {
            clearPendingPermissionRequest(device);
            clearDeniedPermission(device);
            nativeOnUsbAttached(device.getVendorId(), device.getProductId());
            nativeOnUsbPermission(true);
            openUsbDevice(device);
            return;
        }

        if (isPermissionRequestInFlight(device)) {
            if (!hasPermissionRequestTimedOut(device)) {
                return;
            }
            clearPendingPermissionRequest(device);
        }

        if (isPermissionDenied(device)) {
            return;
        }

        nativeOnUsbAttached(device.getVendorId(), device.getProductId());
        markPermissionRequestInFlight(device);
        usbManager.requestPermission(device, usbPermissionIntent);
    }

    private void openUsbDevice(UsbDevice device) {
        closeUsbDevice();

        UsbDeviceConnection connection = usbManager.openDevice(device);
        if (connection == null) {
            nativeOnUsbError("Failed to open USB device " + describeDevice(device));
            return;
        }

        UsbInterface selectedInterface = null;
        UsbInterface selectedControlInterface = null;
        UsbEndpoint inEndpoint = null;
        UsbEndpoint outEndpoint = null;

        for (int i = 0; i < device.getInterfaceCount(); i++) {
            UsbInterface candidateInterface = device.getInterface(i);
            UsbEndpoint candidateIn = null;
            UsbEndpoint candidateOut = null;
            for (int e = 0; e < candidateInterface.getEndpointCount(); e++) {
                UsbEndpoint endpoint = candidateInterface.getEndpoint(e);
                if (endpoint.getType() != UsbConstants.USB_ENDPOINT_XFER_BULK) {
                    continue;
                }
                if (endpoint.getDirection() == UsbConstants.USB_DIR_IN) {
                    candidateIn = endpoint;
                } else if (endpoint.getDirection() == UsbConstants.USB_DIR_OUT) {
                    candidateOut = endpoint;
                }
            }
            if (selectedControlInterface == null && isCdcAcmControlInterface(candidateInterface)) {
                selectedControlInterface = candidateInterface;
            }
            if (selectedInterface == null && candidateIn != null && candidateOut != null) {
                selectedInterface = candidateInterface;
                inEndpoint = candidateIn;
                outEndpoint = candidateOut;
            }
        }

        if (selectedInterface == null || inEndpoint == null || outEndpoint == null) {
            connection.close();
            nativeOnUsbError("USB bulk in/out endpoints were not found for " + describeDevice(device));
            return;
        }

        if (selectedControlInterface != null && selectedControlInterface != selectedInterface) {
            if (!connection.claimInterface(selectedControlInterface, true)) {
                connection.close();
                nativeOnUsbError("Failed to claim USB control interface for " + describeDevice(device));
                return;
            }
        }

        if (!connection.claimInterface(selectedInterface, true)) {
            if (selectedControlInterface != null && selectedControlInterface != selectedInterface) {
                connection.releaseInterface(selectedControlInterface);
            }
            connection.close();
            nativeOnUsbError("Failed to claim USB interface for " + describeDevice(device));
            return;
        }

        if (!configureCdcAcmIfPresent(connection, selectedControlInterface)) {
            if (selectedControlInterface != null && selectedControlInterface != selectedInterface) {
                connection.releaseInterface(selectedControlInterface);
            }
            connection.releaseInterface(selectedInterface);
            connection.close();
            nativeOnUsbError("Failed to configure USB CDC ACM serial parameters for " + describeDevice(device));
            return;
        }

        synchronized (usbLock) {
            currentDevice = device;
            usbConnection = connection;
            usbControlInterface = selectedControlInterface;
            usbInterface = selectedInterface;
            usbInEndpoint = inEndpoint;
            usbOutEndpoint = outEndpoint;
            usbReadRunning = true;
            final UsbDeviceConnection readConnection = connection;
            final UsbEndpoint readEndpoint = inEndpoint;
            usbReadThread = new Thread(() -> runUsbReadLoop(readConnection, readEndpoint), "NiYienUsbRead");
            usbReadThread.start();
        }

        nativeOnUsbOpened(device.getVendorId(), device.getProductId());
    }

    private void runUsbReadLoop(UsbDeviceConnection connection, UsbEndpoint endpoint) {
        byte[] buffer = new byte[512];
        while (usbReadRunning) {
            int read = connection.bulkTransfer(endpoint, buffer, buffer.length, READ_TIMEOUT_MS);
            if (read > 0) {
                byte[] copy = Arrays.copyOf(buffer, read);
                nativeOnUsbBytes(copy, read);
            }
        }
    }

    private boolean writeUsbDeviceBytes(byte[] data) {
        synchronized (usbLock) {
            if (usbConnection == null || usbOutEndpoint == null) {
                nativeOnUsbWriteResult(false, "USB device is not open");
                return false;
            }
            int written = usbConnection.bulkTransfer(usbOutEndpoint, data, data.length, WRITE_TIMEOUT_MS);
            boolean ok = written == data.length;
            if (!ok) {
                nativeOnUsbWriteResult(false, "USB write failed: written=" + written + " expected=" + data.length);
                return false;
            }
            nativeOnUsbWriteResult(true, "");
            return true;
        }
    }

    private void closeUsbDevice() {
        Thread threadToJoin;
        synchronized (usbLock) {
            usbReadRunning = false;
            threadToJoin = usbReadThread;
            usbReadThread = null;
            if (usbConnection != null && usbControlInterface != null && usbControlInterface != usbInterface) {
                try {
                    usbConnection.releaseInterface(usbControlInterface);
                } catch (RuntimeException e) {
                    Log.w(TAG, "USB release control interface failed", e);
                }
            }
            if (usbConnection != null && usbInterface != null) {
                try {
                    usbConnection.releaseInterface(usbInterface);
                } catch (RuntimeException e) {
                    Log.w(TAG, "USB releaseInterface failed", e);
                }
            }
            if (usbConnection != null) {
                usbConnection.close();
            }
            currentDevice = null;
            usbConnection = null;
            usbControlInterface = null;
            usbInterface = null;
            usbInEndpoint = null;
            usbOutEndpoint = null;
        }

        if (threadToJoin != null && threadToJoin != Thread.currentThread()) {
            try {
                threadToJoin.join(200);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
    }

    @SuppressWarnings("deprecation")
    private static UsbDevice getUsbDevice(Intent intent) {
        if (intent == null) {
            return null;
        }
        return (UsbDevice)intent.getParcelableExtra(UsbManager.EXTRA_DEVICE);
    }

    private static Uri extractPickerUri(Intent data) {
        // Prefer single-document URI in Intent.data (most pickers, including
        // MIUI fileexplorer for single-select). Fall back to first ClipData
        // item when the picker fills only the multi-select payload. v1 takes
        // the first item only; multi-select dispatches will be added in v2.
        Uri primary = data.getData();
        if (primary != null) {
            return primary;
        }
        ClipData clip = data.getClipData();
        if (clip != null && clip.getItemCount() > 0) {
            ClipData.Item item = clip.getItemAt(0);
            if (item != null) {
                return item.getUri();
            }
        }
        return null;
    }

    private static boolean isNiyienDevice(UsbDevice device) {
        return device != null
                && device.getVendorId() == NIYIEN_VENDOR_ID
                && device.getProductId() == NIYIEN_PRODUCT_ID;
    }

    private boolean isCurrentDevice(UsbDevice device) {
        return device != null
                && currentDevice != null
                && device.getDeviceId() == currentDevice.getDeviceId();
    }

    private boolean isCurrentDeviceOpen(UsbDevice device) {
        synchronized (usbLock) {
            return isCurrentDevice(device) && usbConnection != null;
        }
    }

    private void markPermissionRequestInFlight(UsbDevice device) {
        synchronized (usbLock) {
            pendingPermissionDevice = device;
            usbPermissionRequestInFlight = true;
            usbPermissionRequestedAtMs = System.currentTimeMillis();
        }
    }

    private void clearPendingPermissionRequest(UsbDevice device) {
        synchronized (usbLock) {
            if (device == null || isPendingPermissionDevice(device)) {
                pendingPermissionDevice = null;
                usbPermissionRequestInFlight = false;
                usbPermissionRequestedAtMs = 0;
            }
        }
    }

    private boolean isPermissionRequestInFlight(UsbDevice device) {
        synchronized (usbLock) {
            return usbPermissionRequestInFlight && isPendingPermissionDevice(device);
        }
    }

    private boolean isPendingPermissionDevice(UsbDevice device) {
        return device != null
                && pendingPermissionDevice != null
                && device.getDeviceId() == pendingPermissionDevice.getDeviceId();
    }

    private boolean hasPermissionRequestTimedOut(UsbDevice device) {
        synchronized (usbLock) {
            return usbPermissionRequestInFlight
                    && isPendingPermissionDevice(device)
                    && usbPermissionRequestedAtMs > 0
                    && System.currentTimeMillis() - usbPermissionRequestedAtMs
                            >= USB_PERMISSION_REQUEST_TIMEOUT_MS;
        }
    }

    private void markDeniedPermission(UsbDevice device) {
        if (!isNiyienDevice(device)) {
            return;
        }
        synchronized (usbLock) {
            deniedPermissionDevice = device;
        }
    }

    private void clearDeniedPermission(UsbDevice device) {
        synchronized (usbLock) {
            if (device == null || isDeniedPermissionDevice(device)) {
                deniedPermissionDevice = null;
            }
        }
    }

    private boolean isPermissionDenied(UsbDevice device) {
        synchronized (usbLock) {
            return isDeniedPermissionDevice(device);
        }
    }

    private boolean isDeniedPermissionDevice(UsbDevice device) {
        return device != null
                && deniedPermissionDevice != null
                && device.getDeviceId() == deniedPermissionDevice.getDeviceId();
    }

    private static boolean isCdcAcmControlInterface(UsbInterface usbInterface) {
        return usbInterface.getInterfaceClass() == UsbConstants.USB_CLASS_COMM
                && usbInterface.getInterfaceSubclass() == 2;
    }

    private static boolean configureCdcAcmIfPresent(
            UsbDeviceConnection connection,
            UsbInterface controlInterface) {
        if (controlInterface == null) {
            return true;
        }

        byte[] lineCoding = new byte[] {
                (byte)(CDC_BAUD_RATE & 0xff),
                (byte)((CDC_BAUD_RATE >> 8) & 0xff),
                (byte)((CDC_BAUD_RATE >> 16) & 0xff),
                (byte)((CDC_BAUD_RATE >> 24) & 0xff),
                0,
                0,
                8
        };

        int lineCodingResult = connection.controlTransfer(
                CDC_REQUEST_TYPE_OUT,
                CDC_SET_LINE_CODING,
                0,
                controlInterface.getId(),
                lineCoding,
                lineCoding.length,
                CONTROL_TIMEOUT_MS);
        if (lineCodingResult != lineCoding.length) {
            return false;
        }

        int controlLineResult = connection.controlTransfer(
                CDC_REQUEST_TYPE_OUT,
                CDC_SET_CONTROL_LINE_STATE,
                CDC_CONTROL_LINE_DTR_RTS,
                controlInterface.getId(),
                null,
                0,
                CONTROL_TIMEOUT_MS);
        return controlLineResult >= 0;
    }

    private static String describeDevice(UsbDevice device) {
        if (device == null) {
            return "null";
        }
        return "name=" + device.getDeviceName()
                + " vid=0x" + Integer.toHexString(device.getVendorId())
                + " pid=0x" + Integer.toHexString(device.getProductId())
                + " interfaces=" + device.getInterfaceCount();
    }
}
