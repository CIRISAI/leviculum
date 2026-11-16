package network.leviculum.loquax;

/**
 * What the join form shows (1448, UI walk 1 finding 4). The form opened from
 * the first start's "Paste a code" with an error sentence and "Try again"
 * before anything was tried: the screen starts the session once without a
 * code, the library answers "scan or paste the code", and that answer was
 * shown as a failure. And it asked for the computer's address and port
 * beside the code, which a code from `mailbox admit` carries already.
 *
 * Now the sentence and Retry come only after a join the user asked for (a
 * Join tap, a shared or scanned code); the start's own look, which needs a
 * code, shows the bare form. The address field lives under "Advanced",
 * shown only while the code carries no server: a code with servers is
 * the code's business (1471), and an empty form has no use for it until
 * the user opens it or a join asks for it.
 */
final class JoinForm {
    private JoinForm() {}

    /**
     * Whether a failed start shows its sentence and Retry: only when the
     * user asked for the join (`asked`). The look without a code that ends in
     * "the form, please" (`need` "code" or "server") is no failure.
     */
    static boolean saysFailure(boolean asked, String need) {
        return asked || !("code".equals(need) || "server".equals(need));
    }

    /** Whether "Advanced" with the address field is there at all: not while the code carries servers. */
    static boolean advancedOffered(String code) {
        return !ServerField.carried(code);
    }

    /**
     * Whether the address field shows under an offered "Advanced": opened by
     * the user, or a join that needs the server (`need` "server"), or a
     * server the user wrote is in it.
     */
    static boolean advancedOpen(boolean opened, String need, String field) {
        return opened || "server".equals(need) || !field.trim().isEmpty();
    }

    /**
     * Whether a join that needs the server failed on the servers the code
     * itself carries (1471, 1449 item 7): the address field is not offered
     * then, so focus goes to the code and the sentence says to check it or
     * ask for a new one, never to a field the user cannot reach.
     */
    static boolean codesServersFailed(String need, String code) {
        return "server".equals(need) && ServerField.carried(code);
    }
}
