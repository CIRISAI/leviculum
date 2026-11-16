package network.leviculum.loquax;

/**
 * The join form's rules (1448, walk finding 4), on the host, run by
 * tests/fling.rs. The start's own look without a code shows no failure;
 * a join the user asked for does. The address field is offered only under
 * a code without servers, and shows there when opened, asked for or
 * already written. Control: the rule before 1448 (every failed start says
 * so) says a failure on the bare form.
 */
final class JoinFormTest {
    private static int checks;

    public static void main(String[] args) {
        check(!JoinForm.saysFailure(false, "code"), "the start's own look says a failure");
        check(!JoinForm.saysFailure(false, "server"), "the start's own look, a server owed");
        check(JoinForm.saysFailure(true, "code"), "a Join tap without a code says so");
        check(JoinForm.saysFailure(true, "server"), "a join that needs the server says so");
        check(JoinForm.saysFailure(false, ""), "a start that failed for another reason says so");
        check(JoinForm.saysFailure(false, "moved"), "a moved mailbox says so");
        check(before(false, "code") && before(false, "code") != JoinForm.saysFailure(false, "code"),
                "control: the rule before 1448 says a failure on the bare form, and this one tells them apart");

        String carried = "lqxh1q2w3e4r@127.0.0.1:4242,home.example:4242";
        check(!JoinForm.advancedOffered(carried), "a code with servers offers no address field");
        check(JoinForm.advancedOffered("lqxh1q2w3e4r"), "a bare code offers it");
        check(JoinForm.advancedOffered(""), "an empty form offers it");
        check(!JoinForm.advancedOpen(false, "", ""), "folded until opened");
        check(JoinForm.advancedOpen(true, "", ""), "opened");
        check(JoinForm.advancedOpen(false, "server", ""), "a join that needs the server opens it");
        check(JoinForm.advancedOpen(false, "", "home.example:4242"), "a server written stays shown");
        // 1449 item 7: a join that needs the server, on a code that carries
        // servers, sends the user to the code; control: a bare code does not.
        check(JoinForm.codesServersFailed("server", carried), "the code's servers failed: edit the code");
        check(!JoinForm.codesServersFailed("server", "lqxh1q2w3e4r"), "control: a bare code needs the field");
        check(!JoinForm.codesServersFailed("code", carried), "a missing code is not the servers' failure");
        System.out.println(checks + " checks passed");
    }

    /** The rule before 1448: every failed start showed its sentence and Retry. */
    private static boolean before(boolean asked, String need) {
        return true;
    }

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }
}
