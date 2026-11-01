package network.leviculum.loquax;

import android.os.Handler;
import android.os.Looper;
import java.util.Map;

/**
 * The library's events, read on a thread of their own and handed on the UI
 * thread to the screen in front and to the service that holds the session.
 */
final class Events {
    interface Listener {
        void onEvent(Map<String, Object> event);
    }

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static Listener listener;
    private static Listener service;
    private static Thread reader;
    /**
     * How long one read waits for an event. Long, so an idle phone is not
     * woken once a second for nothing (1176): every event, the session's
     * end included, returns the read at once, and a read with no session
     * returns "stopped" at once.
     */
    private static final int WAIT_MS = 360_000;

    private Events() {}

    /** The screen in front, or null; UI thread only. */
    static void listen(Listener screen) {
        listener = screen;
    }

    /** The running service, or null; UI thread only. */
    static void service(Listener holder) {
        service = holder;
    }

    /** Read events from now on, once per session. */
    static synchronized void start() {
        if (reader != null) {
            return;
        }
        reader = new Thread(new Runnable() {
            @Override
            public void run() {
                read();
            }
        }, "loquax-events");
        reader.setDaemon(true);
        reader.start();
    }

    private static void read() {
        while (true) {
            final Map<String, Object> event = Core.nextEvent(WAIT_MS);
            if (event == null) {
                continue;
            }
            MAIN.post(new Runnable() {
                @Override
                public void run() {
                    if (listener != null) {
                        listener.onEvent(event);
                    }
                    if (service != null) {
                        service.onEvent(event);
                    }
                }
            });
            if ("stopped".equals(Core.str(event, "t")) || event.containsKey("error")) {
                synchronized (Events.class) {
                    reader = null;
                }
                return;
            }
        }
    }
}
