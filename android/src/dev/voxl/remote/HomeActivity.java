package dev.voxl.remote;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.ResolveInfo;
import android.os.Bundle;
import android.os.SystemClock;
import android.util.Log;

/**
 * The phone's Home button or gesture, when voxl is set as the Home app. Pressed while
 * voxl was in front, it presses the streamed iPhone's Home and brings voxl straight
 * back; pressed anywhere else, it opens the usual launcher (Nova, One UI, ...).
 */
public class HomeActivity extends Activity {
    /** Home this soon after voxl was in front came from inside voxl. */
    private static final long FROM_VOXL_MS = 1500;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        route();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        route();
    }

    private void route() {
        MainActivity main = MainActivity.current;
        long since = SystemClock.uptimeMillis() - MainActivity.leftAt;
        boolean fromVoxl = main != null && (MainActivity.resumed || since < FROM_VOXL_MS);
        Log.i("voxl", "Home: fromVoxl=" + fromVoxl + " sinceLeft=" + since);
        if (fromVoxl) {
            main.device("home");
            startActivity(new Intent(this, MainActivity.class)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_REORDER_TO_FRONT));
        } else {
            toLauncher();
        }
        finish();
        overridePendingTransition(0, 0);
    }

    /** The launcher outside voxl: an installed one (Nova, ...) over the built-in one. */
    private void toLauncher() {
        Intent home = new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME);
        ResolveInfo pick = null;
        for (ResolveInfo r : getPackageManager().queryIntentActivities(home, 0)) {
            String pkg = r.activityInfo.packageName;
            if (pkg.equals(getPackageName()) || pkg.equals("com.android.settings")
                    || r.activityInfo.name.endsWith("FallbackHome")) continue;
            boolean system = (r.activityInfo.applicationInfo.flags & ApplicationInfo.FLAG_SYSTEM) != 0;
            if (pick == null || !system) pick = r;
            if (!system) break;
        }
        if (pick == null) return;
        startActivity(new Intent(home)
                .setComponent(new ComponentName(pick.activityInfo.packageName, pick.activityInfo.name))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    }
}
