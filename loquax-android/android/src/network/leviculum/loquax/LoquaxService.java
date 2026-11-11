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
import android.util.Log;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The foreground service that holds the session while no screen is in
 * front: the process stays, so the leaf node, its link to the home node and
 * the sync schedule's quiet pulls keep running in the library. It shows the
 * persistent notification (connected or not, a stop button) and one
 * notification per conversation with new messages, which opens the chat
 * and carries that conversation's count. Screens bind to it
 * ({@link ServiceLink}) while they are visible, so no message notification
 * is posted over the app itself.
 *
 * Type remoteMessaging: the app carries messages between the phone and its
 * home node. Started sticky: when the system ends the process and starts
 * the service again, it starts the session from the saved home code and
 * address, as the first screen does on a later start. After a reboot
 * {@link BootReceiver} starts it when the user had it running (1345).
 */
public class LoquaxService extends Service implements Events.Listener {
    static final String STOP = "network.leviculum.loquax.STOP";
    /** A message notification's inline reply (1506). */
    static final String REPLY = "network.leviculum.loquax.REPLY";
    private static final String REPLY_TEXT = "reply";
    private static final String REPLY_PEER = "peer";
    private static final String REPLY_KEY = "key";
    static final String SESSION_CHANNEL = "session";
    private static final String MESSAGES_CHANNEL = "messages";
    private static final int SESSION_ID = 1;
    /** How long a failed start waits before the next. */
    private static final long RETRY_MS = 30000;
    /** Where the wish to run survives a reboot: running and not stopped (1345). */
    private static final String PREFS = "service";
    private static final String WANTED = "wanted";

    /** The service in this process once it is in the foreground; null otherwise. */
    private static LoquaxService foreground;

    private final Handler main = new Handler(Looper.getMainLooper());
    private final Binder binder = new Local();
    /** Each conversation's unread count as last seen; null before the first look. */
    private Map<String, Long> unread;
    /**
     * The library's digest of the list's counts at the last look (1464,
     * `changed`'s `counts`): a `changed` that leaves it is no reason to look.
     */
    private Long counts;
    /** Whether the TCP interface to the home node is up, as the library last said. */
    private boolean linkUp;
    /** Whether the mailbox runs here, in this process (1152). */
    private boolean here;
    /** What the persistent notification says now. */
    private Notice.State state = Notice.State.STARTING;
    /** What it said when last posted; null before startForeground. */
    private Notice.State shown;
    private int screens;
    private boolean running;
    private boolean stopping;
    /** The phone's network, told to the library while the session runs (1298). */
    private NetWatch network;

    /** Start or keep the service; from a screen in front, after a start. */
    static void start(Context context) {
        if (foreground != null) {
            // Already in the foreground: a second startForegroundService
            // would want a second startForeground, which posts the notice
            // again and undoes a swipe (1345).
            return;
        }
        context.startForegroundService(new Intent(context, LoquaxService.class));
    }

    /** Whether the user had the service running: it ran and was not stopped. */
    static boolean wanted(Context context) {
        return context.getSharedPreferences(PREFS, MODE_PRIVATE).getBoolean(WANTED, false);
    }

    private static void want(Context context, boolean wanted) {
        context.getSharedPreferences(PREFS, MODE_PRIVATE).edit().putBoolean(WANTED, wanted).apply();
    }

    /**
     * The two channels, made before the first notification and before
     * Settings opens the session channel's system page.
     *
     * The session channel is LOW, and a lower one is not ours to set: a
     * foreground service's notification needs at least LOW
     * (developer.android.com/develop/background-work/services/fgs/launch),
     * and NotificationManagerService lifts an app's MIN or NONE channel of
     * a foreground service to LOW unless the user set the importance
     * (AOSP frameworks/base 8617e4ec9efa2c34bbebda750f29e43d0fdf4044 and
     * 025d4a59fced3af7acfccef9cbae13735ddce7c8). What the user sets there
     * holds, so Settings sends them to the channel's page, where they pick
     * minimised or off (Android 8 to 13, where the notice cannot be
     * swiped, and every later one).
     */
    static void channels(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel(SESSION_CHANNEL,
                context.getString(R.string.channel_session), NotificationManager.IMPORTANCE_LOW));
        manager.createNotificationChannel(new NotificationChannel(MESSAGES_CHANNEL,
                context.getString(R.string.channel_messages), NotificationManager.IMPORTANCE_DEFAULT));
    }

    final class Local extends Binder {
        LoquaxService service() {
            return LoquaxService.this;
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        channels(this);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && STOP.equals(intent.getAction())) {
            end();
            return START_NOT_STICKY;
        }
        if (intent != null && REPLY.equals(intent.getAction())) {
            reply(intent);
        }
        if (foreground != this) {
            // The notice is posted here once; later posts come only from
            // a change of its sentence (show). A start after the Stop
            // began is a new foreground, as before.
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
            shown = state;
            foreground = this;
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
            want(this, true);
            linkUp = Core.bool(answer, "link");
            here = Core.bool(answer, "here");
            Events.start();
            look(false);
            Book.observe(this);
            if (network == null) {
                network = NetWatch.start(this);
            }
            return;
        }
        if (!Core.str(answer, "need").isEmpty()) {
            // Not joined: nothing to hold until the first screen joins.
            end();
            return;
        }
        // The words of the failure are the log's; the line says what the
        // service does about it (walk finding 19).
        Log.i(PhotoView.TAG, "start failed: " + Core.str(answer, "error"));
        show(Notice.State.AWAY);
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
        // Stopped, by its button or for want of a join: a reboot leaves it so.
        want(this, false);
        foreground = null;
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
        if (foreground == this) {
            foreground = null;
        }
        if (network != null) {
            network.stop();
            network = null;
        }
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
            // Only when an unread count, a mute or a share moved (1464): a
            // first pull says `changed` hundreds of times and moves none,
            // and the Pixel's service looked 648 times in 44 s.
            long now = Core.num(event, "counts");
            if (counts == null || counts != now) {
                counts = now;
                look(true);
            }
        } else if ("link".equals(kind)) {
            linkUp = Core.bool(event, "up");
            describe();
        } else if ("stopped".equals(kind) && running) {
            // Ended by itself (the stop button sets stopping first): the
            // library has dropped the dead session, so the next start
            // builds a fresh one.
            running = false;
            Log.i(PhotoView.TAG, "session ended: " + Core.str(event, "detail"));
            show(Notice.State.AWAY);
            if (!stopping) {
                retryLater();
            }
        }
    }

    /**
     * The conversations again: a message notification for each
     * conversation whose unread grew since the last look. The persistent
     * notification carries no count, so this posts it only when the link
     * or the mailbox's place changed with it (1345).
     */
    private void look(boolean tell) {
        // The list never waits for the session's lock (1464).
        Map<String, Object> answer = Core.conversations();
        if (answer.containsKey("error")) {
            return;
        }
        List<Map<String, Object>> rows = Core.list(answer, "conversations");
        Map<String, Long> now = new HashMap<>();
        boolean sharing = false;
        NotificationManager manager = getSystemService(NotificationManager.class);
        // What there is to cancel (1464): a binder call each, 4 s of the
        // Pixel's 44. The first look cancels what an earlier run posted.
        java.util.Set<String> posted = new java.util.HashSet<>();
        if (unread == null) {
            for (android.service.notification.StatusBarNotification shown : manager.getActiveNotifications()) {
                if (shown.getTag() != null) {
                    posted.add(shown.getTag());
                }
            }
        }
        for (Map<String, Object> row : rows) {
            String peer = Core.str(row, "peer");
            sharing |= Core.bool(row, "sharing");
            long count = Core.num(row, "unread");
            now.put(peer, count);
            Long before = unread == null ? null : unread.get(peer);
            if (count == 0) {
                boolean shown = unread == null ? posted.contains(peer) : before != null && before > 0;
                if (shown) {
                    manager.cancel(peer, 0);
                }
            } else if (tell && unread != null && count > (before == null ? 0 : before)
                    && screens == 0 && !Core.bool(row, "muted")) {
                // A muted conversation counts, and posts nothing (1146).
                manager.notify(peer, 0, message(row));
            }
        }
        unread = now;
        ShareService.look(this, sharing);
        describe();
    }

    /** The persistent notification's sentence from the link and the mailbox's place. */
    private void describe() {
        if (!running) {
            return;
        }
        // The screens' link to a mailbox here is a loopback one: the
        // mailbox itself is what the line names. No address on the line
        // (walk finding 19): the profile's Advanced lists the servers.
        show(Notice.of(running, here, linkUp));
    }

    /**
     * The notice says `now`: posted again only when that is a change
     * (Notice.posts), so a swipe on Android 14 and later holds until the
     * next one, and before startForeground nothing is posted.
     */
    private void show(Notice.State now) {
        state = now;
        if (stopping || shown == null || !Notice.posts(shown, now)) {
            return;
        }
        shown = now;
        getSystemService(NotificationManager.class).notify(SESSION_ID, persistent());
    }

    private String line() {
        switch (state) {
            case HERE:
                return getString(R.string.here_running);
            case CONNECTED:
                return getString(R.string.connected);
            case AWAY:
                return getString(R.string.not_connected);
            default:
                return getString(R.string.starting);
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
                .setContentText(line())
                .setContentIntent(opening)
                .setOngoing(true)
                .setShowWhen(false)
                .addAction(new Notification.Action.Builder(null,
                        getString(R.string.stop), stopIt).build())
                .build();
    }

    /**
     * The inline reply of a message notification (1506): the text goes to
     * the library with the key the notification showed, which reads the
     * chat up to it and sends the read receipt with the reply. The
     * notification goes then, as Android wants after an inline reply; one
     * for a message that came since is posted again by the next look.
     */
    private void reply(Intent intent) {
        android.os.Bundle results = android.app.RemoteInput.getResultsFromIntent(intent);
        CharSequence text = results == null ? null : results.getCharSequence(REPLY_TEXT);
        final String peer = intent.getStringExtra(REPLY_PEER);
        final long key = intent.getLongExtra(REPLY_KEY, 0);
        if (peer == null || text == null || text.toString().trim().isEmpty()) {
            return;
        }
        final String body = text.toString();
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.noticeReply(peer, key, body);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                if (error != null) {
                    Log.i(PhotoView.TAG, "notification reply: " + error);
                }
                getSystemService(NotificationManager.class).cancel(peer, 0);
                if (unread != null) {
                    unread.put(peer, 0L);
                }
            }
        });
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
        // The inline reply (1506): it carries the newest message the
        // notification shows, so the reply reads exactly up to it.
        Object newest = row.get("notice_key");
        if (newest instanceof Number) {
            Intent answer = new Intent(this, LoquaxService.class).setAction(REPLY)
                    .putExtra(REPLY_PEER, peer)
                    .putExtra(REPLY_KEY, ((Number) newest).longValue());
            // Mutable: the system writes the typed text into it.
            int mutable = Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0;
            PendingIntent replying = PendingIntent.getService(this, peer.hashCode(), answer,
                    mutable | PendingIntent.FLAG_UPDATE_CURRENT);
            android.app.RemoteInput input = new android.app.RemoteInput.Builder(REPLY_TEXT)
                    .setLabel(getString(R.string.action_reply))
                    .build();
            builder.addAction(new Notification.Action.Builder(null,
                    getString(R.string.action_reply), replying)
                    .addRemoteInput(input)
                    .setAllowGeneratedReplies(false)
                    .build());
        }
        return builder.build();
    }
}
