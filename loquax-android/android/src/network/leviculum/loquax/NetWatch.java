package network.leviculum.loquax;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.PowerManager;
import android.util.Log;

/**
 * The phone's network as the library hears it (1298): Android's default
 * network callback says which network carries the traffic, and the end of
 * Doze says the phone is awake again. Each becomes one {@link Core#network}
 * request with the network's kind and whether it is another network than
 * the one before; what to do about it (drop the dead connection, dial
 * again, ask the paths) is the library's, bounded and idempotent there, so
 * a burst of callbacks here is harmless. No restart, no user action.
 */
final class NetWatch extends ConnectivityManager.NetworkCallback {
    private final Context context;
    private final ConnectivityManager connectivity;
    /** The default network as last said, null for none. */
    private Network last;
    private String lastKind = "none";
    /** Whether a callback came since the registration. */
    private boolean heard;
    private final BroadcastReceiver doze = new BroadcastReceiver() {
        @Override
        public void onReceive(Context ignored, Intent intent) {
            PowerManager power = context.getSystemService(PowerManager.class);
            if (power != null && !power.isDeviceIdleMode()) {
                // Awake: the same network, but its connection may have
                // died while the phone slept.
                say(lastKind, false);
            }
        }
    };

    private NetWatch(Context context) {
        this.context = context.getApplicationContext();
        this.connectivity = this.context.getSystemService(ConnectivityManager.class);
    }

    /** Watch from now on; {@link #stop} ends it. Null where the phone has no ConnectivityManager. */
    static NetWatch start(Context context) {
        NetWatch watch = new NetWatch(context);
        if (watch.connectivity == null) {
            return null;
        }
        try {
            watch.connectivity.registerDefaultNetworkCallback(watch);
        } catch (RuntimeException refused) {
            Log.i(PhotoView.TAG, "no network callback: " + refused);
            return null;
        }
        watch.context.registerReceiver(watch.doze,
                new IntentFilter(PowerManager.ACTION_DEVICE_IDLE_MODE_CHANGED));
        return watch;
    }

    void stop() {
        try {
            connectivity.unregisterNetworkCallback(this);
            context.unregisterReceiver(doze);
        } catch (RuntimeException gone) {
            // Already unregistered.
        }
    }

    /** The word the library takes for a network's kind. */
    static String kind(NetworkCapabilities caps) {
        if (caps == null) {
            return "other";
        }
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) {
            return "wifi";
        }
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) {
            return "mobile";
        }
        return "other";
    }

    @Override
    public void onAvailable(Network network) {
        // The first callback after the registration names the network the
        // session started on: no change for it.
        boolean changed = heard && !network.equals(last);
        heard = true;
        last = network;
        lastKind = kind(connectivity.getNetworkCapabilities(network));
        say(lastKind, changed);
    }

    @Override
    public void onLost(Network network) {
        if (network.equals(last)) {
            last = null;
            lastKind = "none";
            say("none", true);
        }
    }

    private void say(String kind, boolean changed) {
        Log.i(PhotoView.TAG, "network " + kind + (changed ? " changed" : ""));
        // Through the engine's thread (1446), in order with the screens'
        // commands, whichever thread the platform calls back on.
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.network(kind, changed);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                if (error != null) {
                    Log.i(PhotoView.TAG, "network: " + error);
                }
            }
        });
    }
}
