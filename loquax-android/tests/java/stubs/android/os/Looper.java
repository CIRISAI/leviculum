package android.os;

/** The host's stand-in (1496): one main looper, the test's own thread. */
public final class Looper {
    private static final Looper MAIN = new Looper();
    private static volatile Thread mainThread;

    private Looper() {}

    /** The test says which thread is its UI thread. */
    public static void becomeMain() {
        mainThread = Thread.currentThread();
    }

    public static Looper getMainLooper() {
        return MAIN;
    }

    public static Looper myLooper() {
        return Thread.currentThread() == mainThread ? MAIN : null;
    }
}
