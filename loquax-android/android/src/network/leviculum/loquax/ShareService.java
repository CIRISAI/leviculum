package network.leviculum.loquax;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import java.util.List;
import java.util.Map;

/**
 * The foreground service that keeps the phone's position coming while a
 * share runs (1273): the notification "Loquax shares your position" with
 * a Stop, and the fixes to the library. The library decides what goes and
 * when ({@code loquax::ui::share}); this only delivers fixes and keeps the
 * process awake for them, and ends itself once the library lists no share.
 *
 * Type location: Android 14 wants it for a service that reads the
 * position in the background. A start from the background (the share of a
 * restart, before any screen) may be refused there; the share then runs on
 * in the library, on the fixes a screen brings, until a screen starts the
 * service again.
 */
public class ShareService extends Service {
    static final String STOP = "network.leviculum.loquax.STOP_SHARES";
    private static final String CHANNEL = "share";
    private static final int ID = 2;
    /** How often fixes are asked for; the library's shortest gap is a minute. */
    private static final long FIX_MS = 30_000;
    /** How often the library's list is looked at, and the seeded fix read. */
    private static final long LOOK_MS = 15_000;

    /** The running one, for {@link #look}; null while none runs. */
    private static ShareService running;

    private final Handler main = new Handler(Looper.getMainLooper());
    private LocationManager manager;
    private final LocationListener listener = new LocationListener() {
        @Override
        public void onLocationChanged(final Location location) {
            Engine.run(new Engine.Call() {
                @Override
                public String run() {
                    return Locator.tell(location);
                }
            });
        }

        @Override
        public void onStatusChanged(String provider, int status, Bundle extras) {}

        @Override
        public void onProviderEnabled(String provider) {}

        @Override
        public void onProviderDisabled(String provider) {}
    };
    private final Runnable looking = new Runnable() {
        @Override
        public void run() {
            final Runnable again = this;
            check(new Runnable() {
                @Override
                public void run() {
                    Location seeded = Locator.fixture(ShareService.this);
                    if (seeded != null) {
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return Locator.tell(seeded);
                            }
                        });
                    }
                    main.postDelayed(again, LOOK_MS);
                }
            });
        }
    };
    /** The shares as the library listed them last (1464): the notice before it answers again. */
    private List<Map<String, Object>> shares = new java.util.ArrayList<>();

    /** Start it, or keep it: after a share started, and whenever the list says one runs. */
    static void start(Context context) {
        try {
            context.startForegroundService(new Intent(context, ShareService.class));
        } catch (RuntimeException refused) {
            // Not allowed from the background on this Android: the next
            // screen in front asks again.
        }
    }

    /**
     * The list changed (`sharing` on its rows): the service starts with
     * the first share, a share restored after a restart among them, and
     * ends with the last.
     */
    static void look(Context context, boolean sharing) {
        if (sharing && running == null) {
            start(context);
        } else if (running != null) {
            running.check(null);
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        getSystemService(NotificationManager.class).createNotificationChannel(new NotificationChannel(
                CHANNEL, getString(R.string.position_sharing), NotificationManager.IMPORTANCE_LOW));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        boolean stop = intent != null && STOP.equals(intent.getAction());
        try {
            if (Build.VERSION.SDK_INT >= 34) {
                startForeground(ID, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION);
            } else {
                startForeground(ID, notification());
            }
        } catch (RuntimeException refused) {
            stopSelf();
            return START_NOT_STICKY;
        }
        if (stop) {
            // The notification's Stop: every share ends, each with its end
            // report; the list asked on the engine's thread too (1464).
            Engine.run(new Engine.Call() {
                @Override
                public String run() {
                    String said = null;
                    for (Map<String, Object> share : Core.list(Core.shares(), "shares")) {
                        String error = Core.stopShare(Core.str(share, "peer"));
                        said = error != null ? error : said;
                    }
                    return said;
                }
            });
        }
        running = this;
        listen();
        main.removeCallbacks(looking);
        main.post(looking);
        return START_NOT_STICKY;
    }

    /** Fixes from GPS and the network, as often as a share can use them. */
    private void listen() {
        if (manager != null) {
            return;
        }
        if (checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) != PackageManager.PERMISSION_GRANTED
                && checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION)
                        != PackageManager.PERMISSION_GRANTED) {
            return;
        }
        manager = (LocationManager) getSystemService(Context.LOCATION_SERVICE);
        if (manager == null) {
            return;
        }
        for (String provider : new String[] {LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER}) {
            try {
                if (manager.isProviderEnabled(provider)) {
                    manager.requestLocationUpdates(provider, FIX_MS, 0f, listener, Looper.getMainLooper());
                }
            } catch (IllegalArgumentException | SecurityException e) {
                // That provider is not there, or the permission went.
            }
        }
    }

    /**
     * The notification again from the library's list, asked on the engine's
     * thread (1464), then `then` (null for nothing); gone once none runs,
     * and `then` not run.
     */
    private void check(final Runnable then) {
        Engine.ask(new Engine.Ask<List<Map<String, Object>>>() {
            @Override
            public List<Map<String, Object>> ask() {
                return Core.list(Core.shares(), "shares");
            }
        }, new Engine.Got<List<Map<String, Object>>>() {
            @Override
            public void got(List<Map<String, Object>> now) {
                if (running != ShareService.this) {
                    return;
                }
                shares = now;
                if (now.isEmpty()) {
                    end();
                    return;
                }
                getSystemService(NotificationManager.class).notify(ID, notification(now));
                if (then != null) {
                    then.run();
                }
            }
        });
    }

    private void end() {
        running = null;
        main.removeCallbacks(looking);
        if (manager != null) {
            manager.removeUpdates(listener);
            manager = null;
        }
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        if (running == this) {
            running = null;
        }
        main.removeCallbacksAndMessages(null);
        if (manager != null) {
            manager.removeUpdates(listener);
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    /** The notice from the shares listed last: the library is asked after it (1464). */
    private Notification notification() {
        return notification(shares);
    }

    /** "Loquax shares your position", with whom and how long, and Stop. */
    private Notification notification(List<Map<String, Object>> shares) {
        StringBuilder line = new StringBuilder();
        for (Map<String, Object> share : shares) {
            if (line.length() > 0) {
                line.append('\n');
            }
            line.append(Core.str(share, "name")).append(": ").append(Core.str(share, "words"));
        }
        PendingIntent stopping = PendingIntent.getService(this, 0,
                new Intent(this, ShareService.class).setAction(STOP),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        PendingIntent opening = PendingIntent.getActivity(this, 0,
                new Intent(this, MainActivity.class)
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.ic_menu_mylocation)
                .setContentTitle(getString(R.string.share_notice))
                .setContentText(line.toString())
                .setStyle(new Notification.BigTextStyle().bigText(line.toString()))
                .setContentIntent(opening)
                .setOngoing(true)
                .setShowWhen(false)
                .addAction(new Notification.Action.Builder(null,
                        getString(R.string.share_stop), stopping).build())
                .build();
    }
}
