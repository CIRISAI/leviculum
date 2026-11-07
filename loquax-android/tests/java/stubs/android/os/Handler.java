package android.os;

import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;

/** The host's stand-in (1496): what is posted waits for the test to run it. */
public class Handler {
    private static final LinkedBlockingQueue<Runnable> POSTED = new LinkedBlockingQueue<>();

    public Handler(Looper looper) {}

    public final boolean post(Runnable runnable) {
        return POSTED.add(runnable);
    }

    /** Run the next posted runnable on the caller's thread; false if none came within `ms`. */
    public static boolean runNext(long ms) throws InterruptedException {
        Runnable next = POSTED.poll(ms, TimeUnit.MILLISECONDS);
        if (next == null) {
            return false;
        }
        next.run();
        return true;
    }
}
