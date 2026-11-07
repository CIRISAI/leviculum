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
        check("home.example:4242".equals(typed(CODE, "home.example:4242", false)),
                "the user's server stays: " + typed(CODE, "home.example:4242", false));
        check("".equals(typed("lqxh1q2w3e4r5t6y7u8i9o0p", "", false)), "no server in the code");
        // A typo after the @ taken back: the field follows the code back.
        String field = ServerField.follow("lqxh1@12", "", "");
        field = ServerField.follow("lqxh1@1", field, "12");
        check("1".equals(field), "back to " + field);
        System.out.println(checks + " checks passed");
    }

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
