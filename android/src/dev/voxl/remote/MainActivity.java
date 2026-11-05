package dev.voxl.remote;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Color;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.net.Uri;
import android.os.Bundle;
import android.text.InputType;
import android.util.Log;
import android.view.KeyEvent;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.EditText;

/**
 * voxl's web client full screen, with the phone's hardware keys forwarded to the
 * streamed iOS Simulator: volume up/down press the iPhone's volume buttons and the
 * camera key takes a screenshot. (Browsers never hand these keys to a web page.)
 * Folding the phone locks the iPhone and unfolding wakes it, even with the app in
 * the background. Rotation is handled by the page itself.
 */
public class MainActivity extends Activity {
    private static final String PREFS = "voxl";
    private static final String KEY_URL = "url";
    /** Holding a volume key repeats; don't flood the Simulator. */
    private static final long REPEAT_MS = 120;

    private WebView web;
    private long lastKey;
    private SensorManager sensors;
    /** Null until the hinge sensor reports; then whether the phone is folded shut. */
    private Boolean folded;
    private boolean lockedByFold;

    /**
     * The Fold's hinge angle: about 0 degrees shut, 180 open. Closing the phone is
     * seen while the app is still on screen; reopening is seen when the app comes
     * back (registering an on-change sensor reports its current value).
     */
    private final SensorEventListener hinge = new SensorEventListener() {
        @Override public void onSensorChanged(SensorEvent e) {
            float angle = e.values[0];
            // Hysteresis: a half-open phone (laptop posture) changes nothing.
            if (angle < 30) setFolded(true, angle);
            else if (angle > 120) setFolded(false, angle);
        }
        @Override public void onAccuracyChanged(Sensor s, int a) {}
    };

    private void setFolded(boolean now, float angle) {
        if (folded != null && folded == now) return;
        boolean first = folded == null;
        folded = now;
        Log.i("voxl", "hinge " + angle + " folded=" + now + " lockedByFold=" + lockedByFold);
        if (first && !now) return;
        if (now) {
            device("lock");
            lockedByFold = true;
        } else if (lockedByFold) {
            // The iPhone's side button toggles: a second press wakes it.
            device("lock");
            lockedByFold = false;
        }
    }

    private void device(String action) {
        web.evaluateJavascript("window.voxlDevice && window.voxlDevice('" + action + "')", null);
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        WebView.setWebContentsDebuggingEnabled(true);
        web = new WebView(this);
        web.setBackgroundColor(Color.BLACK);
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setMediaPlaybackRequiresUserGesture(false);
        s.setSupportZoom(false);
        // Tells the page the app handles folding (so it doesn't too).
        s.setUserAgentString(s.getUserAgentString() + " VoxlApp/0.1");
        web.setWebViewClient(new WebViewClient());
        web.setWebChromeClient(new WebChromeClient());
        setContentView(web);
        immersive();
        sensors = getSystemService(SensorManager.class);
        String url = urlFrom(getIntent());
        if (url != null) web.loadUrl(url);
        else askForUrl();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        String url = urlFrom(intent);
        if (url != null) web.loadUrl(url);
    }

    @Override
    public void onWindowFocusChanged(boolean focus) {
        super.onWindowFocusChanged(focus);
        if (focus) immersive();
    }

    private void immersive() {
        WindowInsetsController c = getWindow().getInsetsController();
        if (c == null) return;
        c.hide(WindowInsets.Type.systemBars());
        c.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
    }

    /** The link from the intent (saved for next time), else the saved one. */
    private String urlFrom(Intent intent) {
        SharedPreferences p = getSharedPreferences(PREFS, MODE_PRIVATE);
        Uri data = intent != null ? intent.getData() : null;
        if (data != null && "https".equals(data.getScheme())) {
            p.edit().putString(KEY_URL, data.toString()).apply();
            return data.toString();
        }
        return p.getString(KEY_URL, null);
    }

    private void askForUrl() {
        EditText field = new EditText(this);
        field.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        field.setHint("https://host:port/?token=…");
        new AlertDialog.Builder(this)
                .setTitle("voxl link")
                .setMessage("The link voxl printed (with ?token=)")
                .setView(field)
                .setCancelable(false)
                .setPositiveButton("Open", (d, w) -> {
                    String url = field.getText().toString().trim();
                    if (!url.startsWith("https://")) url = "https://" + url;
                    getSharedPreferences(PREFS, MODE_PRIVATE).edit().putString(KEY_URL, url).apply();
                    web.loadUrl(url);
                })
                .show();
    }

    private static String actionFor(int keyCode) {
        switch (keyCode) {
            case KeyEvent.KEYCODE_VOLUME_UP: return "volup";
            case KeyEvent.KEYCODE_VOLUME_DOWN: return "voldown";
            case KeyEvent.KEYCODE_CAMERA: return "screenshot";
            default: return null;
        }
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent e) {
        String action = actionFor(e.getKeyCode());
        if (action == null) return super.dispatchKeyEvent(e);
        if (e.getAction() == KeyEvent.ACTION_DOWN) {
            long now = System.currentTimeMillis();
            if (e.getRepeatCount() == 0 || now - lastKey >= REPEAT_MS) {
                lastKey = now;
                device(action);
            }
        }
        // Consumed either way: the phone's own volume doesn't change.
        return true;
    }

    @Override
    protected void onResume() {
        super.onResume();
        Sensor h = sensors.getDefaultSensor(Sensor.TYPE_HINGE_ANGLE);
        if (h != null) {
            sensors.unregisterListener(hinge);
            sensors.registerListener(hinge, h, SensorManager.SENSOR_DELAY_NORMAL);
        }
    }

    @Override
    protected void onDestroy() {
        sensors.unregisterListener(hinge);
        super.onDestroy();
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) web.goBack();
        else super.onBackPressed();
    }
}
