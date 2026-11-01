package network.leviculum.loquax;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Binder;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The foreground service that holds the session while no screen is in
 * front: the process stays, so the leaf node, its link to the home node and
 * the sync schedule's quiet pulls keep running in the library. It shows the
 * persistent notification (the home node, the unread count, a stop button)
 * and one notification per conversation with new messages, which opens the
 * chat. Screens bind to it ({@link ServiceLink}) while they are visible, so
 * no message notification is posted over the app itself.
 *
 * Type remoteMessaging: the app carries messages between the phone and its
 * home node. Started sticky: when the system ends the process and starts
 * the service again, it starts the session from the saved home code and
 * address, as the first screen does on a later start.
 */
public class LoquaxService extends Service implements Events.Listener {
    static final String STOP = "network.leviculum.loquax.STOP";
    private static final String SESSION_CHANNEL = "session";
    private static final String MESSAGES_CHANNEL = "messages";
    private static final int SESSION_ID = 1;
    /** How long a failed start waits before the next. */
    private static final long RETRY_MS = 30000;

    private final Handler main = new Handler(Looper.getMainLooper());
    private final Binder binder = new Local();
    /** Each conversation's unread count as last seen; null before the first look. */
    private Map<String, Long> unread;
    private long total;
    private String server = "";
    /** Whether the TCP interface to the home node is up, as the library last said. */
    private boolean linkUp;
    /** Whether the mailbox runs here, in this process (1152). */
    private boolean here;
    private String line;
    private int screens;
    private boolean running;
    private boolean stopping;

    /** Start or keep the service; from a screen in front, after a start. */
    static void start(Context context) {
        context.startForegroundService(new Intent(context, LoquaxService.class));
    }

    final class Local extends Binder {
        LoquaxService service() {
            return LoquaxService.this;
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel(SESSION_CHANNEL,
                getString(R.string.channel_session), NotificationManager.IMPORTANCE_LOW));
        manager.createNotificationChannel(new NotificationChannel(MESSAGES_CHANNEL,
                getString(R.string.channel_messages), NotificationManager.IMPORTANCE_DEFAULT));
        line = getString(R.string.starting);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && STOP.equals(intent.getAction())) {
            end();
            return START_NOT_STICKY;
        }
        try {
            if (Build.VERSION.SDK_INT >= 34) {
                startForeground(SESSION_ID, persistent(),
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING);
            } else {
                startForeground(SESSION_ID, persistent());
            }
        } catch (RuntimeException refused) {
            // Android 12 and later refuse a start from the background
            // without an exemption; the app's next start asks again.
            stopSelf();
            return START_NOT_STICKY;
        }
        Events.service(this);
        if (!running) {
            begin();
        }
        return START_STICKY;
    }

    /** The session: the running one, or one from what the first screen saved. */
    private void begin() {
        final String home = getFilesDir().getAbsolutePath();
        new Thread(new Runnable() {
            @Override
            public void run() {
                Core.sense(getApplicationContext());
                final Map<String, Object> answer = Core.start(home, null, null, null);
                main.post(new Runnable() {
                    @Override
                    public void run() {
                        started(answer);
                    }
                });
            }
        }, "loquax-service-start").start();
    }

    private void started(Map<String, Object> answer) {
        if (stopping) {
            return;
        }
        if (Core.bool(answer, "ok")) {
            running = true;
            server = Core.str(answer, "server");
            linkUp = Core.bool(answer, "link");
            here = Core.bool(answer, "here");
            Events.start();
            look(false);
            Book.observe(this);
            return;
        }
        if (!Core.str(answer, "need").isEmpty()) {
            // Not joined: nothing to hold until the first screen joins.
            end();
            return;
        }
        line = getString(R.string.not_connected, Core.str(answer, "error"));
        show();
        retryLater();
    }

    /** Start again in a while: after a failed start, and after the session died. */
    private void retryLater() {
        main.postDelayed(new Runnable() {
            @Override
            public void run() {
                if (!running && !stopping) {
                    begin();
                }
            }
        }, RETRY_MS);
    }

    /** The stop button: the session ends, the notifications go, the screens close. */
    private void end() {
        if (stopping) {
            return;
        }
        stopping = true;
        running = false;
        new Thread(new Runnable() {
            @Override
            public void run() {
                Native.stop();
            }
        }, "loquax-stop").start();
        getSystemService(NotificationManager.class).cancelAll();
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        Events.service(null);
        main.removeCallbacksAndMessages(null);
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return binder;
    }

    /** A screen came to the front or left it. */
    void screen(boolean shown) {
        screens = Math.max(0, screens + (shown ? 1 : -1));
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            look(true);
        } else if ("link".equals(kind)) {
            linkUp = Core.bool(event, "up");
            describe();
        } else if ("stopped".equals(kind) && running) {
            // Ended by itself (the stop button sets stopping first): the
            // library has dropped the dead session, so the next start
            // builds a fresh one.
            running = false;
            String detail = Core.str(event, "detail");
            line = getString(R.string.not_connected,
                    detail.isEmpty() ? getString(R.string.stopped) : detail);
            show();
            if (!stopping) {
                retryLater();
            }
        }
    }

    /**
     * The conversations again: the unread count for the persistent
     * notification, and a message notification for each conversation
     * whose unread grew since the last look.
     */
    private void look(boolean tell) {
        Map<String, Object> answer = Core.conversations(null);
        if (answer.containsKey("error")) {
            return;
        }
        List<Map<String, Object>> rows = Core.list(answer, "conversations");
        Map<String, Long> now = new HashMap<>();
        long total = 0;
        NotificationManager manager = getSystemService(NotificationManager.class);
        for (Map<String, Object> row : rows) {
            String peer = Core.str(row, "peer");
            long count = Core.num(row, "unread");
            now.put(peer, count);
            total += count;
            Long before = unread == null ? null : unread.get(peer);
            if (count == 0) {
                manager.cancel(peer, 0);
            } else if (tell && unread != null && count > (before == null ? 0 : before)
                    && screens == 0 && !Core.bool(row, "muted")) {
                // A muted conversation counts, and posts nothing (1146).
                manager.notify(peer, 0, message(row));
            }
        }
        unread = now;
        this.total = total;
        describe();
    }

    /** The persistent notification's line: the link, then the unread count. */
    private void describe() {
        if (!running) {
            return;
        }
        if (here) {
            // The screens' link is a loopback one: the mailbox itself is
            // what the line names.
            line = total == 0 ? getString(R.string.here_running)
                    : getString(R.string.here_running_unread, total);
        } else if (!linkUp) {
            line = getString(R.string.connecting, server);
        } else if (total == 0) {
            line = getString(R.string.connected, server);
        } else {
            line = getString(R.string.connected_unread, server, total);
        }
        show();
    }

    private void show() {
        if (!stopping) {
            getSystemService(NotificationManager.class).notify(SESSION_ID, persistent());
        }
    }

    private Notification persistent() {
        Intent stop = new Intent(this, LoquaxService.class).setAction(STOP);
        PendingIntent stopIt = PendingIntent.getService(this, 0, stop,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Intent open = new Intent(this, MainActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent opening = PendingIntent.getActivity(this, 0, open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(this, SESSION_CHANNEL)
                .setSmallIcon(android.R.drawable.stat_notify_sync_noanim)
                .setContentTitle(getString(R.string.app_name))
                .setContentText(line)
                .setContentIntent(opening)
                .setOngoing(true)
                .setShowWhen(false)
                .addAction(new Notification.Action.Builder(null,
                        getString(R.string.stop), stopIt).build())
                .build();
    }

    /** New messages in one conversation: its name and last message; opens the chat. */
    private Notification message(Map<String, Object> row) {
        String peer = Core.str(row, "peer");
        Intent list = new Intent(this, ConversationsActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        Intent chat = new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, peer);
        PendingIntent opening = PendingIntent.getActivities(this, peer.hashCode(),
                new Intent[] {list, chat},
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification.Builder builder = new Notification.Builder(this, MESSAGES_CHANNEL)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle(Core.str(row, "name"))
                .setContentText(Ui.preview(this, row))
                .setNumber((int) Math.min(Integer.MAX_VALUE, Core.num(row, "unread")))
                .setContentIntent(opening)
                .setAutoCancel(true);
        // The linked contact's photo, loquax's own copy: it needs no
        // permission that may have been taken back since (1139).
        android.graphics.Bitmap photo = AvatarView.photo(Core.str(row, "photo"));
        if (photo != null) {
            builder.setLargeIcon(photo);
        }
        return builder.build();
    }
}
