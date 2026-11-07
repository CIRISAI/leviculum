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
 *
 * A code from `mailbox admit` carries several servers, the rendezvous
 * ladder, comma-separated (1402b). Those are the code's business (1471):
 * the field shows them and is read-only while the code carries any, and the
 * join request names no server, so the library takes the code's list whole.
 * Before 1471 the field sent the list as one server and the library refused
 * it, "Loquax kann nicht lesen, wo das Postfach ist".
 */
final class ServerField {
    private ServerField() {}

    /** The code's server, after its `@`; empty when it names none. */
    static String tail(String code) {
        int at = code.indexOf('@');
        return at < 0 ? "" : code.substring(at + 1).trim();
    }

    /**
     * Whether the code carries its own servers (1471): then they are the
     * code's business, the field only shows them and takes no typing.
     */
    static boolean carried(String code) {
        return !tail(code).isEmpty();
    }

    /**
     * What the field holds after the code became `code`: `field` is what it
     * holds now and `given` what the code gave it last ("" for nothing).
     * A code that carries servers shows them all (1471), the whole list
     * `mailbox admit` printed; a field the user wrote keeps it while the
     * code carries none.
     */
    static String follow(String code, String field, String given) {
        if (carried(code)) {
            return tail(code);
        }
        String now = field.trim();
        if (!now.isEmpty() && !now.equals(given)) {
            return field;
        }
        return "";
    }

    /**
     * The server the join request names (1471): none while the code carries
     * its servers, so the library walks the code's whole list in its order;
     * otherwise the one the user typed, null for none. A list typed by hand
     * goes as typed and the library refuses it, naming one address.
     */
    static String request(String code, String field) {
        if (carried(code)) {
            return null;
        }
        String typed = field.trim();
        return typed.isEmpty() ? null : typed;
    }
}
