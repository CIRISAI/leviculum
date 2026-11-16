package network.leviculum.loquax;

/**
 * What Back does in a chat (UI walk 1 finding 5, 1449 item 1). A Back meant
 * to close the keyboard dropped the reply being written, silently, and the
 * message then went out without its quote. As WhatsApp has it, a reply
 * stays until its "x" or Send: Back closes a sheet, then a selection, then
 * the keyboard, then the screen, and never ends the reply. Leaving the
 * screen keeps it too, in the draft (1219).
 */
final class BackRule {
    private BackRule() {}

    enum Step { SHEET, SELECTION, KEYBOARD, LEAVE }

    /**
     * The step a Back takes. `reply` is whether a reply is being written:
     * it is a parameter so the rule says, and its test shows, that it
     * changes nothing.
     */
    static Step next(boolean sheet, boolean selection, boolean keyboard, boolean reply) {
        if (sheet) {
            return Step.SHEET;
        }
        if (selection) {
            return Step.SELECTION;
        }
        if (keyboard) {
            return Step.KEYBOARD;
        }
        return Step.LEAVE;
    }
}
