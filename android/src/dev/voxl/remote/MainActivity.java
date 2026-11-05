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
    /** The running screen, for HomeActivity. */
    static MainActivity current;
    /** Resumed: voxl is the app in front. */
    static boolean resumed;
    /** When voxl last stopped being the app in front (uptime ms). */
    static long leftAt = -1_000_000;

    /** The hinge posture last sent: "closed", "half" or "open" (null before the first). */
    private String posture;

    /**
     * The Fold's hinge angle: about 0 degrees shut, 180 flat open. It's sent as a
     * posture: the iPhone Duo simulator folds the same way, any other iPhone locks
     * when shut and wakes when opened (the server decides). Closing is seen while the
     * app is still on screen; reopening when it comes back (registering an on-change
     * sensor reports its current value).
     */
    private final SensorEventListener hinge = new SensorEventListener() {
        @Override public void onSensorChanged(SensorEvent e) {
            float angle = e.values[0];
            // Hysteresis around each boundary, so a hand-held hinge doesn't flicker.
            String next = posture;
            if (angle < 25) next = "closed";
            else if (angle > 160) next = "open";
            else if (angle > 40 && angle < 145) next = "half";
            if (next == null || next.equals(posture)) return;
            boolean first = posture == null;
            posture = next;
            Log.i("voxl", "hinge " + angle + " -> " + next);
            // At launch, only a phone that isn't simply open says anything.
            if (first && next.equals("open")) return;
            device("posture-" + next);
        }
        @Override public void onAccuracyChanged(Sensor s, int a) {}
    };

    void device(String action) {
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
        current = this;
        String url = urlFrom(getIntent());
        if (url != null) web.loadUrl(url);
        else askForUrl();
    }

    @Override
    protected void onPause() {
        if (resumed) leftAt = android.os.SystemClock.uptimeMillis();
        resumed = false;
        super.onPause();
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
        resumed = true;
        Sensor h = sensors.getDefaultSensor(Sensor.TYPE_HINGE_ANGLE);
        if (h != null) {
            sensors.unregisterListener(hinge);
            sensors.registerListener(hinge, h, SensorManager.SENSOR_DELAY_NORMAL);
        }
    }

    @Override
    protected void onDestroy() {
        sensors.unregisterListener(hinge);
        if (current == this) current = null;
        super.onDestroy();
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) web.goBack();
        else super.onBackPressed();
    }
}
