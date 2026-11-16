package network.leviculum.loquax;

/**
 * The chat's Back (1449 item 1, walk finding 5), on the host, run by
 * tests/fling.rs. A reply being written survives every Back: the keyboard
 * closes first, then the screen leaves with the reply in the draft.
 * Control: the rule before 1449 ended the reply on Back.
 */
final class BackRuleTest {
    private static int checks;

    public static void main(String[] args) {
        check(BackRule.next(true, true, true, true) == BackRule.Step.SHEET, "a sheet closes first");
        check(BackRule.next(false, true, true, true) == BackRule.Step.SELECTION, "then a selection");
        check(BackRule.next(false, false, true, true) == BackRule.Step.KEYBOARD,
                "a reply with the keyboard open: the keyboard closes, the reply stays");
        check(BackRule.next(false, false, false, true) == BackRule.Step.LEAVE,
                "a reply with the keyboard closed: the screen leaves, the reply stays in the draft");
        for (boolean keyboard : new boolean[] {false, true}) {
            check(BackRule.next(false, false, keyboard, true) == BackRule.next(false, false, keyboard, false),
                    "a reply changes nothing about Back");
        }
        check(before(false, false, true),
                "control: the rule before 1449 ended the reply in the very state checked above");
        System.out.println(checks + " checks passed");
    }

    /** The rule before 1449: with no sheet and no selection, Back ended the reply. */
    private static boolean before(boolean sheet, boolean selection, boolean reply) {
        return !sheet && !selection && reply;
    }

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }
}
