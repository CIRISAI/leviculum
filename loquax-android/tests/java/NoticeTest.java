package network.leviculum.loquax;

/**
 * The service notice's rules (1345) on the host, run by tests/fling.rs:
 * the sentence follows the link and the mailbox's place, a change of it
 * posts the notice again and a same sentence does not (an unread count
 * moving is no change: the sentence carries none), and a reboot starts
 * the service only when the user had it running.
 */
final class NoticeTest {
    private static int checks;

    public static void main(String[] args) {
        Notice.State c = Notice.State.CONNECTED;
        Notice.State a = Notice.State.AWAY;
        Notice.State h = Notice.State.HERE;
        Notice.State s = Notice.State.STARTING;

        check(Notice.of(true, false, true) == c, "running, link up: connected");
        check(Notice.of(true, false, false) == a, "running, link down: not connected");
        check(Notice.of(true, true, false) == h, "the mailbox here, whatever the link says");
        check(Notice.of(true, true, true) == h, "the mailbox here, link up");
        check(Notice.of(false, false, true) == a, "a dead session is not connected");
        check(Notice.of(false, true, true) == a, "a dead session here is not connected");

        check(Notice.posts(null, s), "startForeground posts it");
        check(Notice.posts(s, c), "starting -> connected posts");
        check(Notice.posts(c, a), "connected -> not connected posts");
        check(Notice.posts(a, c), "not connected -> connected posts");
        check(Notice.posts(s, h), "starting -> here posts");
        check(!Notice.posts(c, c), "connected stays connected: no post");
        check(!Notice.posts(a, a), "a second link-down: no post");
        check(!Notice.posts(h, h), "here stays here: no post");

        // An unread change as the service sees it: the conversations are
        // looked at again, the link and the place are what they were.
        Notice.State before = Notice.of(true, false, true);
        Notice.State after = Notice.of(true, false, true);
        check(!Notice.posts(before, after), "an unread change posts nothing");

        check(Notice.startsAtBoot(Notice.BOOT_COMPLETED, true), "boot, wanted: starts");
        check(!Notice.startsAtBoot(Notice.BOOT_COMPLETED, false), "boot, stopped by the user: stays");
        check(!Notice.startsAtBoot("android.intent.action.LOCKED_BOOT_COMPLETED", true),
                "a locked boot: the files are not readable yet");
        check(!Notice.startsAtBoot("android.intent.action.TIME_SET", true), "another broadcast");
        check(!Notice.startsAtBoot(null, true), "no action");

        System.out.println(checks + " checks passed");
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
