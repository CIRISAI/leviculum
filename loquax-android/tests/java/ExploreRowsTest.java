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
 *
 * Walk 2 of 1621: between looking and tapping, ten new pages arrive, some
 * above the screen, some on it; the row under a planned tap stays at its
 * pixel through all ten, scrolled down and at the very top, and the chip
 * counts the pages that wait. The control, the list as ListView keeps it
 * by position, puts another row under the finger.
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

        ExploreRows<String> rows = new ExploreRows<String>(KEYS);
        rows.offer(Arrays.asList("Radio RPi4", "Bergwacht", "IranTalk", "Werkstatt"));
        rows.down();
        boolean now = rows.offer(Arrays.asList("IranTalk", "Radio RPi4", "Bergwacht", "Werkstatt"));
        check(!now, "held while the finger is down");
        check("IranTalk".equals(rows.at(2)), "the row under the finger: " + rows.at(2));
        check(rows.up(), "the waiting order shows on the lift");
        check("IranTalk".equals(rows.at(0)), "the new order: " + rows.at(0));
        check(!rows.up(), "nothing waits after");
        // The control: without the hold the tap lands on the new third row.
        ExploreRows<String> free = new ExploreRows<String>(KEYS);
        free.offer(Arrays.asList("Radio RPi4", "Bergwacht", "IranTalk", "Werkstatt"));
        free.offer(Arrays.asList("IranTalk", "Radio RPi4", "Bergwacht", "Werkstatt"));
        check("Bergwacht".equals(free.at(2)), "the walk's mis-tap: " + free.at(2));

        stillBetweenLookingAndTapping();
        theTopAtRestFills();
        aTapOpensTheRowItHit();

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

    /** A row's key is the row; a page counts for the chip. */
    private static final ExploreRows.Keys<String> KEYS = new ExploreRows.Keys<String>() {
        @Override
        public String key(String row) {
            return row;
        }

        @Override
        public boolean counts(String row) {
            return row.startsWith("page:");
        }
    };

    /** The rows' heights in pixels, as the pane draws them at 2.75 density. */
    private static int height(String row) {
        if (row.startsWith("title:")) {
            return 110;
        }
        if (row.startsWith("heading:")) {
            return 66;
        }
        return 154;
    }

    /** The screen: the list's rows, the first on screen and its top's pixel. */
    private static final class Screen {
        List<String> rows;
        int first;
        int top;
        final int height = 1800;

        /** The row under pixel {@code y} of the list. */
        String at(int y) {
            int pixel = top;
            for (int i = first; i < rows.size(); i++) {
                int h = height(rows.get(i));
                if (y >= pixel && y < pixel + h) {
                    return rows.get(i);
                }
                pixel += h;
            }
            return null;
        }

        /** The last row on screen. */
        int last() {
            int pixel = top;
            int i = first;
            while (i < rows.size() - 1 && pixel + height(rows.get(i)) < height) {
                pixel += height(rows.get(i));
                i++;
            }
            return i;
        }
    }

    /** The Explore list: its title, "Heute" and pages. */
    private static List<String> explore(int pages) {
        List<String> out = new ArrayList<String>();
        out.add("title:pages");
        out.add("heading:Heute");
        for (int i = 0; i < pages; i++) {
            out.add("page:" + i);
        }
        out.add("title:rooms");
        return out;
    }

    private static void stillBetweenLookingAndTapping() {
        for (boolean atTop : new boolean[] {false, true}) {
            ExploreRows<String> rows = new ExploreRows<String>(KEYS);
            Screen screen = new Screen();
            screen.rows = explore(40);
            rows.offer(screen.rows);
            screen.first = atTop ? 0 : 14;
            screen.top = atTop ? 0 : -40;
            // The control: ListView by position, the same answers.
            Screen control = new Screen();
            control.rows = screen.rows;
            control.first = screen.first;
            control.top = screen.top;
            int tap = 900;
            String aimed = screen.at(tap);
            check(aimed != null && aimed.startsWith("page:"), "a page under the tap: " + aimed);
            List<String> next = new ArrayList<String>(screen.rows);
            for (int n = 0; n < 10; n++) {
                // Even ones at the top of "Heute", above the screen when
                // it is scrolled; odd ones in the middle of the screen.
                int where = n % 2 == 0 ? 2 : next.indexOf(aimed) - 1;
                next.add(where, "page:new" + n);
                int said = rows.offer(next, screen.first, screen.last(), false);
                if (said == ExploreRows.SHOWN) {
                    screen.rows = new ArrayList<String>(next);
                    if (rows.anchor() >= 0) {
                        screen.first = rows.anchor();
                    }
                }
                check(aimed.equals(screen.at(tap)),
                        (atTop ? "at the top" : "scrolled") + ", insert " + n + ": "
                                + screen.at(tap) + " under the tap, not " + aimed);
                control.rows = new ArrayList<String>(next);
            }
            // Scrolled, the first page lands above the screen and shows
            // unseen; from the first one on screen on, every answer waits.
            int waiting = atTop ? 10 : 9;
            check(rows.fresh() == waiting, "the chip counts " + waiting + ": " + rows.fresh());
            check(!aimed.equals(control.at(tap)), "the control moves the row: " + control.at(tap));
            // The chip: everything shows, from the top.
            check(rows.apply(), "the chip applies");
            check(rows.size() == next.size(), "all of them: " + rows.size());
            check(rows.fresh() == 0, "nothing waits after the chip");
        }
        // The reader's own verb shows at once, whatever it moves.
        ExploreRows<String> own = new ExploreRows<String>(KEYS);
        own.offer(explore(5));
        check(own.offer(explore(9), 0, 6, true) == ExploreRows.SHOWN, "a search shows at once");
        // A finger holds, and the lift offers under the same rule.
        own.down();
        List<String> newer = explore(9);
        newer.addAll(2, Arrays.asList("page:a", "page:b", "page:c"));
        check(own.offer(newer, 0, 6, false) == ExploreRows.HELD, "held under the finger");
        check(own.up(0, 6) == ExploreRows.WAITING, "the lift: it would move rows on screen");
        check(own.fresh() == 3, "three new: " + own.fresh());
        // Pages after the screen's last row move nothing on it: shown.
        ExploreRows<String> below = new ExploreRows<String>(KEYS);
        below.offer(explore(9));
        check(below.offer(explore(12), 0, 6, false) == ExploreRows.SHOWN, "below the screen");
        // Only the line under a row changed: shown at once, nothing moves.
        ExploreRows<String> same = new ExploreRows<String>(KEYS);
        same.offer(explore(5));
        check(same.offer(explore(5), 0, 6, false) == ExploreRows.SHOWN, "the same rows show");
        check(same.anchor() == 0, "where they stood");
    }

    /**
     * Walk 3 of 1629: after a cold start the list stood with one page and
     * the chip counted to seven while nobody touched anything. At the very
     * top, untouched for {@link ExploreRows#QUIET_MS}, every answer shows
     * at once and the list stays at its top; touched a moment ago, or
     * scrolled, it waits behind the chip as before. The control, the 1621
     * rule, keeps the cold start's pages behind the chip.
     */
    private static void theTopAtRestFills() {
        ExploreRows<String> rows = new ExploreRows<String>(KEYS);
        ExploreRows<String> control = new ExploreRows<String>(KEYS);
        rows.offer(explore(1));
        control.offer(explore(1));
        long now = 10_000;
        for (int n = 2; n <= 7; n++) {
            List<String> next = explore(n);
            int said = rows.offer(next, 0, next.size() - 1, false, true, now);
            check(said == ExploreRows.SHOWN, "at rest at the top, page " + n + " shows: " + said);
            check(rows.anchor() == 0, "the list stays at its top: " + rows.anchor());
            check(rows.size() == next.size(), "all of them: " + rows.size());
            control.offer(next, 0, next.size() - 1, false);
            now += 5_000;
        }
        check(rows.fresh() == 0, "no chip: " + rows.fresh());
        check(control.fresh() == 6, "the control's chip counts six: " + control.fresh());
        // Touched a moment ago at the top: the reader aims, it waits.
        List<String> above = explore(7);
        above.add(2, "page:top");
        rows.touched(now - 1_000);
        check(rows.offer(above, 0, 6, false, true, now) == ExploreRows.WAITING,
                "touched a second ago, it waits");
        // Untouched long enough since: it shows.
        check(rows.offer(above, 0, 6, false, true, now - 1_000 + ExploreRows.QUIET_MS)
                == ExploreRows.SHOWN, "at rest again, it shows");
        // Scrolled, at rest: it waits, as 1621 has it.
        List<String> more = new ArrayList<String>(above);
        more.add(5, "page:new");
        check(rows.offer(more, 4, 8, false, false, now + 60_000) == ExploreRows.WAITING,
                "scrolled, it waits");
        // A finger on the list holds, at the top or not.
        rows.down();
        check(rows.offer(more, 0, 6, false, true, now + 60_000) == ExploreRows.HELD,
                "held under the finger");
    }

    /**
     * Walk 4 of 1632, new 11: a tap on "Ben's Library" opened the row
     * above it, twice. ListView takes the position under the finger when
     * it comes down and clicks it a moment after the lift; an answer that
     * lands in between (the lift's own "touch" verb draws) and inserts a
     * page above the screen is shown at once, anchored, so the click's
     * position now names the row above. The click opens the row the finger
     * came down on, by its key, never by position in the newer list.
     */
    private static void aTapOpensTheRowItHit() {
        // The order's case: A, B, C shown, the library re-sorts to B, A, C
        // (the screen unknown, so it shows), the finger on row 2.
        ExploreRows<String> rows = new ExploreRows<String>(KEYS);
        rows.offer(Arrays.asList("page:A", "page:B", "page:C"));
        rows.press(1);
        rows.down();
        rows.up();
        rows.offer(Arrays.asList("page:B", "page:A", "page:C"));
        String hit = rows.hit(1);
        check("page:B".equals(hit), "the tap on row 2 opens B, the row the finger hit: " + hit);
        check("page:A".equals(rows.at(1)), "the control, by position: " + rows.at(1));
        // The walk's case: scrolled, the finger on a page, the lift, then
        // one page lands at the top of "Heute", above the screen.
        ExploreRows<String> walk = new ExploreRows<String>(KEYS);
        List<String> list = explore(40);
        walk.offer(list);
        int first = 14;
        int tap = 18;
        String aimed = list.get(tap);
        walk.press(tap);
        walk.down();
        check(walk.up(first, first + 10) == ExploreRows.HELD, "nothing waited for the lift");
        List<String> next = new ArrayList<String>(list);
        next.add(2, "page:heard");
        check(walk.offer(next, first, first + 10, false) == ExploreRows.SHOWN,
                "a page above the screen shows at once, unseen");
        check(!aimed.equals(walk.at(tap)), "the control lands one row up: " + walk.at(tap));
        hit = walk.hit(tap);
        check(aimed.equals(hit), "the click opens " + aimed + ", not " + hit);
        // A click with no finger before it (a key, accessibility) is the
        // row at its position, and a press is used once.
        check(walk.hit(5).equals(walk.at(5)), "a click without a press");
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
