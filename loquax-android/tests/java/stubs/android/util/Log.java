package android.util;

/** The host's stand-in (1496): the log goes to stderr. */
public final class Log {
    private Log() {}

    public static int w(String tag, String message) {
        System.err.println("W/" + tag + ": " + message);
        return 0;
    }

    public static int e(String tag, String message) {
        System.err.println("E/" + tag + ": " + message);
        return 0;
    }

    public static int e(String tag, String message, Throwable thrown) {
        System.err.println("E/" + tag + ": " + message + ": " + thrown);
        return 0;
    }
}
