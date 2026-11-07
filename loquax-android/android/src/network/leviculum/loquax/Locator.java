package network.leviculum.loquax;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.DialogInterface;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * The phone's own position (1201), by Android's LocationManager: no Play
 * services, no AndroidX. The rules of the navigation page:
 *
 * The permission is asked only when the user taps the crosshair or "Share
 * my position", never at start, and a short line before the system's
 * dialog says why. Refused, the caller goes on without a fix; the next tap
 * may ask again as long as Android lets it.
 *
 * Reading sends nothing. A position leaves the phone only through
 * {@link #share}, which a tap on "Share my position", "Share mine" or the
 * chat's attachment menu calls, to one peer.
 *
 * The position is read while the map is in front (watch, stop) and once
 * for a share; GPS and the network provider both, the newest fix wins.
 */
final class Locator {
    /** The request code of the permission question. */
    static final int ASK = 0x10c1;
    /**
     * In a debuggable build, a file of this name in the app's files holding
     * `lat,lon` in degrees is the phone's fix, taken now (1270): the device
     * walk's seeded fix, since an emulator has no position to give.
     */
    static final String FIXTURE = "fix-fixture.txt";
    /** A fix younger than this is fresh enough to share or centre on. */
    private static final long FRESH_MS = 2 * 60_000;
    /** How long a share waits for a fix before it says there is none. */
    private static final long WAIT_MS = 30_000;
    /** How often and how far apart the map hears of a new fix. */
    private static final long WATCH_MS = 5_000;
    private static final float WATCH_M = 5f;

    /** A fix, or null for none. */
    interface Fix {
        void on(Location location);
    }

    private final Activity activity;
    /** Android's, fetched on first use: an activity has none before onCreate. */
    private LocationManager manager;
    private final Handler main = new Handler(Looper.getMainLooper());
    private Runnable granted;
    private Runnable refused;
    private Fix watcher;
    private final LocationListener watching = new Listener() {
        @Override
        public void onLocationChanged(Location location) {
            if (watcher != null) {
                watcher.on(location);
            }
        }
    };

    Locator(Activity activity) {
        this.activity = activity;
    }

    /** Android's location manager, null on a phone without one. */
    private LocationManager manager() {
        if (manager == null) {
            manager = (LocationManager) activity.getSystemService(Context.LOCATION_SERVICE);
        }
        return manager;
    }

    /** Whether the phone lets Loquax read the position, fine or coarse. */
    boolean allowed() {
        return activity.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION)
                        == PackageManager.PERMISSION_GRANTED
                || activity.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION)
                        == PackageManager.PERMISSION_GRANTED;
    }

    /**
     * Make sure of the permission, then `onGranted`; `onRefused` when the
     * user says not now or the system refuses. Asks with the line that
     * says why first.
     */
    void ask(final Runnable onGranted, final Runnable onRefused) {
        if (allowed()) {
            onGranted.run();
            return;
        }
        new AlertDialog.Builder(activity)
                .setMessage(R.string.location_why)
                .setPositiveButton(R.string.location_continue, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        granted = onGranted;
                        refused = onRefused;
                        activity.requestPermissions(new String[] {
                            Manifest.permission.ACCESS_FINE_LOCATION,
                            Manifest.permission.ACCESS_COARSE_LOCATION,
                        }, ASK);
                    }
                })
                .setNegativeButton(R.string.location_not_now, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        onRefused.run();
                    }
                })
                .setOnCancelListener(new DialogInterface.OnCancelListener() {
                    @Override
                    public void onCancel(DialogInterface dialog) {
                        onRefused.run();
                    }
                })
                .show();
    }

    /** The system's answer, from the activity's onRequestPermissionsResult: whether it was ours. */
    boolean result(int request) {
        if (request != ASK) {
            return false;
        }
        Runnable next = allowed() ? granted : refused;
        granted = null;
        refused = null;
        if (next != null) {
            next.run();
        }
        return true;
    }

    /** The providers to read: GPS and the network's, those that are on. */
    private List<String> providers() {
        List<String> out = new ArrayList<>();
        LocationManager manager = manager();
        for (String provider : new String[] {LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER}) {
            try {
                if (manager != null && manager.isProviderEnabled(provider)) {
                    out.add(provider);
                }
            } catch (IllegalArgumentException e) {
                // A phone without that provider.
            }
        }
        return out;
    }

    /** The newest fix the phone holds, or null. */
    Location last() {
        LocationManager manager = manager();
        if (!allowed() || manager == null) {
            return null;
        }
        Location best = null;
        for (String provider : providers()) {
            try {
                Location fix = manager.getLastKnownLocation(provider);
                if (fix != null && (best == null || fix.getTime() > best.getTime())) {
                    best = fix;
                }
            } catch (SecurityException e) {
                return null;
            }
        }
        return best;
    }

    /** The debug build's seeded fix ({@link #FIXTURE}), or null. */
    Location fixture() {
        return fixture(activity);
    }

    /** The seeded fix in `context`'s files, read anew: the share service's too (1273). */
    static Location fixture(Context context) {
        boolean debuggable =
                (context.getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
        File file = new File(context.getFilesDir(), FIXTURE);
        if (!debuggable || !file.isFile()) {
            return null;
        }
        byte[] bytes = new byte[64];
        int n;
        try (FileInputStream in = new FileInputStream(file)) {
            n = in.read(bytes);
        } catch (IOException e) {
            return null;
        }
        String[] parts = new String(bytes, 0, Math.max(n, 0), StandardCharsets.US_ASCII).trim().split(",");
        if (parts.length != 2) {
            return null;
        }
        try {
            Location fix = new Location("fixture");
            fix.setLatitude(Double.parseDouble(parts[0].trim()));
            fix.setLongitude(Double.parseDouble(parts[1].trim()));
            fix.setAccuracy(5f);
            fix.setTime(System.currentTimeMillis());
            return fix;
        } catch (NumberFormatException e) {
            return null;
        }
    }

    /** Whether `fix` is recent enough to centre on or share. */
    static boolean fresh(Location fix) {
        return fix != null && System.currentTimeMillis() - fix.getTime() < FRESH_MS;
    }

    /**
     * One fresh fix for `fix`: the phone's last when it is fresh, else the
     * first that comes within WAIT_MS, else the last one at all (null when
     * there is none). The permission must be there.
     */
    void once(final Fix fix) {
        Location known = last();
        if (fresh(known)) {
            fix.on(known);
            return;
        }
        List<String> providers = providers();
        final LocationManager manager = manager();
        if (providers.isEmpty() || manager == null) {
            fix.on(known);
            return;
        }
        final boolean[] done = {false};
        final LocationListener[] one = new LocationListener[1];
        final Runnable give = new Runnable() {
            @Override
            public void run() {
                if (done[0]) {
                    return;
                }
                done[0] = true;
                manager.removeUpdates(one[0]);
                fix.on(last());
            }
        };
        one[0] = new Listener() {
            @Override
            public void onLocationChanged(Location location) {
                if (done[0]) {
                    return;
                }
                done[0] = true;
                main.removeCallbacks(give);
                manager.removeUpdates(this);
                fix.on(location);
            }
        };
        try {
            for (String provider : providers) {
                manager.requestLocationUpdates(provider, 0, 0, one[0], Looper.getMainLooper());
            }
        } catch (SecurityException e) {
            done[0] = true;
            fix.on(null);
            return;
        }
        main.postDelayed(give, WAIT_MS);
    }

    /** Hear of each new fix while the map is in front; the last known first. */
    void watch(Fix fix) {
        stop();
        LocationManager manager = manager();
        if (!allowed() || manager == null) {
            return;
        }
        watcher = fix;
        Location known = last();
        if (known != null) {
            fix.on(known);
        }
        try {
            for (String provider : providers()) {
                manager.requestLocationUpdates(provider, WATCH_MS, WATCH_M, watching, Looper.getMainLooper());
            }
        } catch (SecurityException e) {
            watcher = null;
        }
    }

    /** Stop hearing: the map left the front. */
    void stop() {
        watcher = null;
        if (manager != null) {
            // Only once fetched: nothing was asked of a manager not yet had.
            manager.removeUpdates(watching);
        }
    }

    /**
     * Send `fix` to `peer`, the user asked for it: the library's share,
     * a report of ours with the fix alone. The error in words, or null.
     */
    /**
     * The phone's newest fix to the library, for the compose bar's position
     * sheet (1270); null says there is none. The error in words, or null.
     */
    static String tell(Location fix) {
        if (fix == null) {
            return Core.noFix();
        }
        Long alt = fix.hasAltitude() ? Math.round(fix.getAltitude() * 100) : 0L;
        Long accuracy = fix.hasAccuracy() ? Math.round(fix.getAccuracy() * 100.0) : 0L;
        return Core.command(Core.map("t", "fix",
                "lat_e6", Math.round(fix.getLatitude() * 1e6),
                "lon_e6", Math.round(fix.getLongitude() * 1e6),
                "alt_e2", alt,
                "accuracy_e2", accuracy,
                "at", fix.getTime() / 1000));
    }

    static String share(String peer, Location fix) {
        Long alt = fix.hasAltitude() ? Math.round(fix.getAltitude() * 100) : 0L;
        Long accuracy = fix.hasAccuracy() ? Math.round(fix.getAccuracy() * 100.0) : 0L;
        return Core.command(Core.map("t", "share", "peer", peer,
                "lat_e6", Math.round(fix.getLatitude() * 1e6),
                "lon_e6", Math.round(fix.getLongitude() * 1e6),
                "alt_e2", alt,
                "accuracy_e2", accuracy,
                "at", fix.getTime() / 1000));
    }

    /**
     * The whole share as a tap does it (1201): the permission if need be,
     * a fresh fix, the send to `peer` named `name`, and one line on how it
     * went, through `say`.
     */
    void shareWith(final String peer, final String name, final Fix done, final Say say) {
        ask(new Runnable() {
            @Override
            public void run() {
                say.line(activity.getString(R.string.location_finding));
                once(new Fix() {
                    @Override
                    public void on(Location location) {
                        if (location == null) {
                            say.line(activity.getString(R.string.location_none));
                            return;
                        }
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return share(peer, location);
                            }
                        }, new Engine.Said() {
                            @Override
                            public void said(String error) {
                                say.line(error == null
                                        ? activity.getString(R.string.shared_to, name)
                                        : activity.getString(R.string.share_failed, error));
                                if (error == null && done != null) {
                                    done.on(location);
                                }
                            }
                        });
                    }
                });
            }
        }, new Runnable() {
            @Override
            public void run() {
                say.line(activity.getString(R.string.location_off));
            }
        });
    }

    /** Where a share says how it went. */
    interface Say {
        void line(String text);
    }

    /** A listener that only hears of fixes; the rest is Android's old noise. */
    private abstract static class Listener implements LocationListener {
        @Override
        public void onStatusChanged(String provider, int status, Bundle extras) {}

        @Override
        public void onProviderEnabled(String provider) {}

        @Override
        public void onProviderDisabled(String provider) {}
    }
}
