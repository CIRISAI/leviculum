package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * The Explore pane's rules (1618), on the host, run by tests/fling.rs.
 *
 * Walk 1: 32 characters typed fast, each a verb whose answer comes back
 * after the next characters were typed. Under the rule the field holds
 * all 32; under the rule before 1618 (the control) it is set back to an
 * older answer and loses characters, as the Pixel lost 10 of 32.
 *
 * Walk 3: a finger comes down on the third row, a new order arrives; the
 * third row is still the one the finger came down on, and the new order
 * shows when it lifts.
 */
final class ExploreRowsTest {
    private static int checks;

    public static void main(String[] args) {
        String typed = "0123456789abcdef0123456789abcdef";
        check(typed.equals(type(typed, false)), "all 32: " + type(typed, false));
        String before = type(typed, true);
        check(before.length() < typed.length(), "the control loses characters: " + before);
        // The library's own word still reaches the field: cleared, with
        // nothing on its way.
        check(ExploreRows.echo("", 0, "abc"), "a field the library cleared");
        check(!ExploreRows.echo("ab", 1, "abc"), "an older answer while a verb is on its way");

        ExploreRows<String> rows = new ExploreRows<String>();
        rows.offer(Arrays.asList("Radio RPi4", "Bergwacht", "IranTalk", "Werkstatt"));
        rows.down();
        boolean now = rows.offer(Arrays.asList("IranTalk", "Radio RPi4", "Bergwacht", "Werkstatt"));
        check(!now, "held while the finger is down");
        check("IranTalk".equals(rows.at(2)), "the row under the finger: " + rows.at(2));
        check(rows.up(), "the waiting order shows on the lift");
        check("IranTalk".equals(rows.at(0)), "the new order: " + rows.at(0));
        check(!rows.up(), "nothing waits after");
        // The control: without the hold the tap lands on the new third row.
        ExploreRows<String> free = new ExploreRows<String>();
        free.offer(Arrays.asList("Radio RPi4", "Bergwacht", "IranTalk", "Werkstatt"));
        free.offer(Arrays.asList("IranTalk", "Radio RPi4", "Bergwacht", "Werkstatt"));
        check("Bergwacht".equals(free.at(2)), "the walk's mis-tap: " + free.at(2));

        check(ExploreRows.id("page:aa") == ExploreRows.id("page:aa"), "an id is stable");
        check(ExploreRows.id("page:aa") != ExploreRows.id("page:ab"), "two rows, two ids");
        System.out.println(checks + " checks passed");
    }

    /**
     * Type `text` one character at a time; each character is a verb, and
     * the answer of verb n arrives after character n + 2 was typed (the
     * engine's thread behind the keyboard). The field as it ends.
     */
    private static String type(String text, boolean ruleBefore) {
        String field = "";
        int inFlight = 0;
        List<String> answers = new ArrayList<String>();
        for (int i = 0; i < text.length(); i++) {
            field = field + text.charAt(i);
            inFlight++;
            answers.add(field);
            if (answers.size() > 2) {
                String answer = answers.remove(0);
                inFlight--;
                field = apply(answer, inFlight, field, ruleBefore);
            }
        }
        while (!answers.isEmpty()) {
            String answer = answers.remove(0);
            inFlight--;
            field = apply(answer, inFlight, field, ruleBefore);
        }
        return field;
    }

    private static String apply(String answer, int inFlight, String field, boolean ruleBefore) {
        boolean take = ruleBefore ? ExploreRows.echoBefore(answer, field)
                : ExploreRows.echo(answer, inFlight, field);
        return take ? answer : field;
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
