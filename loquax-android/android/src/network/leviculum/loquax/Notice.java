package network.leviculum.loquax;

/**
 * The service notice's rules (1345), plain Java so the host runs them
 * (tests/fling.rs with tests/java/NoticeTest.java): which sentence the
 * persistent notification says, when it is posted again, and whether a
 * reboot starts the service.
 *
 * From Android 14 on the user may swipe the notice away
 * (developer.android.com/about/versions/14/behavior-changes-all). Every
 * notify() of it brings it back, so it is posted again only when its
 * sentence changes: the mailbox here, connected, not connected. The unread
 * count is not on it; each conversation's notification carries its own.
 */
final class Notice {
    /** What the notice says; one sentence each. */
    enum State {
        /** The service started, the session is not up yet. */
        STARTING,
        /** The mailbox runs in this process (1152). */
        HERE,
        /** The link to the home node is up. */
        CONNECTED,
        /** No link: a failed start, a dead session, or the link down. */
        AWAY,
    }

    static final String BOOT_COMPLETED = "android.intent.action.BOOT_COMPLETED";

    private Notice() {}

    /** The sentence for the session as the service last heard of it. */
    static State of(boolean running, boolean here, boolean linkUp) {
        if (!running) {
            return State.AWAY;
        }
        if (here) {
            return State.HERE;
        }
        return linkUp ? State.CONNECTED : State.AWAY;
    }

    /**
     * Whether the notice is posted again when its sentence goes from
     * `shown` to `now`: only on a change, so a swipe holds until the next
     * one. `shown` is null before startForeground posted it.
     */
    static boolean posts(State shown, State now) {
        return shown != now;
    }

    /**
     * Whether a broadcast starts the service: the end of a boot, and only
     * when the user had it running, that is, it was running and not
     * stopped by its Stop.
     */
    static boolean startsAtBoot(String action, boolean wanted) {
        return wanted && BOOT_COMPLETED.equals(action);
    }
}
