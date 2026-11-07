package network.leviculum.loquax;

/**
 * The join screen's server field follows the code's tail (1464). A code
 * carries its server after `@` (`lqxh1...@host:port`), and the field takes
 * it from there. Before 1464 the field took it only while it was empty: a
 * code typed one character at a time, by a keyboard or `adb shell input
 * text`, filled it with the first character after the `@` (a stray "1" of
 * "127.0.0.1") and left it so. Now the field follows the code as long as it
 * still holds what it was given from the code; what the user typed into it
 * stays.
 */
final class ServerField {
    private ServerField() {}

    /** The code's server, after its `@`; empty when it names none. */
    static String tail(String code) {
        int at = code.indexOf('@');
        return at < 0 ? "" : code.substring(at + 1).trim();
    }

    /**
     * What the field holds after the code became `code`: `field` is what it
     * holds now and `given` what the code gave it last ("" for nothing).
     * A field the user wrote keeps it.
     */
    static String follow(String code, String field, String given) {
        String now = field.trim();
        if (!now.isEmpty() && !now.equals(given)) {
            return field;
        }
        return tail(code);
    }
}
