package network.leviculum.loquax;

/**
 * The join screen's server field (1464), on the host, run by
 * tests/fling.rs: a code typed one character at a time, as the bench's
 * `adb shell input text` and any keyboard type it, ends with the whole
 * server in the field; the rule before 1464 (fill once, while empty) ends
 * with its first character, the stray "1" the bench saw. A pasted code
 * fills it at once, a server the user wrote stays, and a code without a
 * server leaves the field as the user left it.
 */
final class ServerFieldTest {
    private static int checks;

    static final String CODE = "lqxh1q2w3e4r5t6y7u8i9o0p@127.0.0.1:4242";

    public static void main(String[] args) {
        check("127.0.0.1:4242".equals(typed(CODE, "", false)),
                "typed: " + typed(CODE, "", false));
        check("1".equals(typed(CODE, "", true)),
                "the rule before 1464 ends with " + typed(CODE, "", true));
        check("127.0.0.1:4242".equals(ServerField.follow(CODE, "", "")), "pasted");
        // A code that carries servers shows them, over what the user wrote (1471).
        check("127.0.0.1:4242".equals(typed(CODE, "home.example:4242", false)),
                "the code's server shows: " + typed(CODE, "home.example:4242", false));
        check("home.example:4242".equals(typed(BARE, "home.example:4242", false)),
                "the user's server stays under a bare code: " + typed(BARE, "home.example:4242", false));
        check("".equals(typed(BARE, "", false)), "no server in the code");
        // A typo after the @ taken back: the field follows the code back.
        String field = ServerField.follow("lqxh1@12", "", "");
        field = ServerField.follow("lqxh1@1", field, "12");
        check("1".equals(field), "back to " + field);
        // The code taken back past its @: the field it filled empties.
        field = ServerField.follow("lqxh1", field, field);
        check("".equals(field), "emptied: " + field);

        // 1471, the Pixel 2026-11-07: a code typed with three servers. The
        // field shows all three, read-only, and the join request names no
        // server: the code goes whole, its three servers in their order,
        // and the library dials them as the code lists them.
        String shown = typed(THREE, "", false);
        check(SERVERS.equals(shown), "three shown: " + shown);
        check(ServerField.carried(THREE), "read-only while the code carries servers");
        check(ServerField.request(THREE, shown) == null,
                "the request names no server: " + ServerField.request(THREE, shown));
        check(ServerField.tail(THREE).equals(SERVERS), "the code carries all three in order");
        // Control: the request before 1471 sent the field as the server,
        // a list the library refuses ("Loquax kann nicht lesen, wo ...").
        check(shown.trim().contains(","), "the field before 1471 sent a list");
        // A code with one server: the same.
        check(ServerField.request(CODE, typed(CODE, "", false)) == null, "one server, none named");
        // A server typed by hand with no code server: it goes.
        check(!ServerField.carried(BARE), "a bare code leaves the field editable");
        check("home.example:4242".equals(ServerField.request(BARE, " home.example:4242 ")),
                "typed by hand: " + ServerField.request(BARE, " home.example:4242 "));
        check(ServerField.request(BARE, "  ") == null, "nothing typed, nothing named");
        // A list typed by hand goes as typed; the library refuses it with
        // the sentence naming one address (session.rs one_server).
        check("a.example:4242,b.example:4242".equals(
                ServerField.request(BARE, "a.example:4242,b.example:4242")), "a typed list goes as typed");
        System.out.println(checks + " checks passed");
    }

    static final String BARE = "lqxh1q2w3e4r5t6y7u8i9o0p";
    static final String SERVERS = "192.168.178.61:4343,leviculum.network:4343,leviculum.network:4242";
    static final String THREE = BARE + "@" + SERVERS;

    /** The field after `code` was typed one character at a time into a field holding `field`. */
    static String typed(String code, String field, boolean before) {
        String given = "";
        for (int i = 1; i <= code.length(); i++) {
            String now = code.substring(0, i);
            if (before) {
                if (field.trim().isEmpty() && !ServerField.tail(now).isEmpty()) {
                    field = ServerField.tail(now);
                }
                continue;
            }
            String next = ServerField.follow(now, field, given);
            if (!next.equals(field)) {
                given = next;
            }
            field = next;
        }
        return field;
    }

    static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }
}
