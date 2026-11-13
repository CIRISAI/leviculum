package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
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
        theScrolledListHoldsStill();
        aWaitingAnswerStillRefreshesTheWords();
        aTapWorksWhileTheChipShows();
        atRestAtTheTopARefreshAlsoSorts();
        aWaitingAnswerWithFewerRowsKeepsTheOthers();
        backFromAPageKeepsTheRowsAndTheChip();
        aTapOnTheSectionRowSurvivesAnAnswerBeforeItsClick();

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
        // Since 1659 (walk 10, 51) the answer waits for the owed click,
        // which the adapter told meanwhile would have cost.
        check(walk.offer(next, first, first + 10, false) == ExploreRows.HELD,
                "a page above the screen waits for the click");
        hit = walk.hit(tap);
        check(aimed.equals(hit), "the click opens " + aimed + ", not " + hit);
        check(walk.settle(first, first + 10) == ExploreRows.SHOWN,
                "then it shows at once, unseen");
        check(!aimed.equals(walk.at(tap)), "the control, by position, lands one row up: " + walk.at(tap));
        // A click with no finger before it (a key, accessibility) is the
        // row at its position, and a press is used once.
        check(walk.hit(5).equals(walk.at(5)), "a click without a press");
    }

    /**
     * Walk 5 of 1639, new 21: untouched, the scrolled list crept down, the
     * "Server" heading from y 561 to 911 in 60 s, in steps of exactly
     * 50 px (20 dp at 2.5) per answer. The list has a 20 dp top band
     * (1632, {@code clipToPadding false}); a row's {@code getTop()} counts
     * from the list's edge, {@code setSelectionFromTop}'s {@code y} from
     * inside the band. The screen here is ListView's arithmetic: answers
     * land above the screen and show at once, anchored; the first row on
     * screen stays on its pixel. The control, the row's top handed back as
     * it was read, moves it one band down per answer.
     */
    private static void theScrolledListHoldsStill() {
        final int band = 50;
        for (int startTop : new int[] {-40, 10, band}) {
            ExploreRows<String> rows = new ExploreRows<String>(KEYS);
            List<String> list = explore(40);
            rows.offer(list);
            int first = 14;
            int top = startTop;
            int controlTop = startTop;
            String aimed = list.get(first);
            for (int n = 0; n < 10; n++) {
                list = new ArrayList<String>(list);
                list.add(2, "page:heard" + n);
                int said = rows.offer(list, first, first + 10, false);
                check(said == ExploreRows.SHOWN, "a page above the screen shows: " + said);
                first = rows.anchor();
                check(aimed.equals(list.get(first)), "the anchor is the row on top: " + list.get(first));
                // ListView: the row lands at the band plus y.
                top = band + ExploreRows.fromTop(top, band);
                controlTop = band + controlTop;
            }
            check(top == startTop, "the first row stays at " + startTop + ", not " + top);
            check(controlTop == startTop + 10 * band,
                    "the control drifts one band per answer: " + controlTop);
        }
    }

    /** A row "key|words": the key before the bar. */
    private static final ExploreRows.Keys<String> WORDS = new ExploreRows.Keys<String>() {
        @Override
        public String key(String row) {
            int bar = row.indexOf('|');
            return bar < 0 ? row : row.substring(0, bar);
        }

        @Override
        public boolean counts(String row) {
            return row.startsWith("page:");
        }
    };

    /**
     * Walk 5 of 1639, new 26: scrolled, a server row said "vor 19 Min. ·
     * über 8 Stationen" while its sheet, drawn from the newest answer,
     * said "gerade eben · über 5 Stationen". The answer that would move a
     * row waited behind the chip whole, so the rows on screen kept the
     * words of an older one. Now a waiting or held answer still gives
     * every row it shares with the screen its words, in place: the same
     * rows, the same order, the newest words. The control is the rule
     * before, which keeps the old words.
     */
    private static void aWaitingAnswerStillRefreshesTheWords() {
        List<String> before = Arrays.asList("title:pages|Seiten", "heading:Heute|Heute",
                "page:A|vor 3 Min.", "page:B|vor 5 Min.", "title:rooms|Räume",
                "server:X|vor 19 Min. · über 8 Stationen");
        List<String> after = Arrays.asList("title:pages|Seiten", "heading:Heute|Heute",
                "page:A|vor 3 Min.", "page:new|gerade eben", "page:B|vor 5 Min.",
                "title:rooms|Räume", "server:X|gerade eben · über 5 Stationen");
        ExploreRows<String> rows = new ExploreRows<String>(WORDS);
        rows.offer(before);
        int said = rows.offer(after, 2, 5, false);
        check(said == ExploreRows.WAITING, "a page among the screen's rows moves them: " + said);
        check("server:X|gerade eben · über 5 Stationen".equals(rows.at(5)),
                "the server row says what its sheet says: " + rows.at(5));
        check("page:A|vor 3 Min.".equals(rows.at(2)), "no row moved: " + rows.at(2));
        check(rows.size() == before.size(), "nothing added: " + rows.size());
        check(rows.fresh() == 1, "the chip still counts the new page: " + rows.fresh());
        // Held under a finger: the same.
        ExploreRows<String> held = new ExploreRows<String>(WORDS);
        held.offer(before);
        held.down();
        check(held.offer(after, 2, 5, false) == ExploreRows.HELD, "held");
        check(held.at(5).endsWith("über 5 Stationen"), "held, the words: " + held.at(5));
        check(held.at(2).startsWith("page:A"), "held, nothing moved: " + held.at(2));
        check(held.up(2, 5) == ExploreRows.WAITING, "the lift: it would move rows");
        check(held.fresh() == 1, "one new page waits: " + held.fresh());
    }

    /**
     * ListView's side of a tap, as AOSP's {@code AbsListView} has it: the
     * press takes the position; {@code notifyDataSetChanged} sets
     * {@code mDataChanged} until the next layout; {@code onTouchUp} and the
     * click it posts drop the click while {@code mDataChanged} holds. The
     * pane's lift ({@code ExplorePane.finger}) runs before ListView's own
     * {@code ACTION_UP}, so a notification it asks for there is between
     * the press and {@code onTouchUp}.
     */
    private static final class Tap {
        boolean dataChanged;

        /** The pane hears the lift: what it does to the adapter. */
        void lift(ExploreRows<String> rows, int first, int last) {
            int said = rows.up(first, last);
            if (ExploreRows.notifies(said)) {
                dataChanged = true;
            }
        }

        /** An answer from the library reaches the pane while the finger is down. */
        void answer(ExploreRows<String> rows, List<String> answer, int first, int last) {
            int said = rows.offer(answer, first, last, false);
            if (ExploreRows.notifies(said)) {
                dataChanged = true;
            }
        }

        /** ListView's onTouchUp: the click, or nothing. */
        String click(ExploreRows<String> rows, int position) {
            return dataChanged ? null : rows.hit(position);
        }
    }

    /**
     * Walk 8 of 1653, 39: while the chip "N neue Seiten" shows, a tap on a
     * row opened nothing, 4 of 4 (ValleiRug twice, FabLab, Root Access
     * Hub); without the chip 4 of 4 opened. The chip means an answer
     * waits; the lift offered it again, it waited again, and the pane
     * told the adapter the words changed: {@code notifyDataSetChanged}
     * between the press and ListView's {@code onTouchUp}, which then
     * drops the click. Three cases: the chip up and nothing new during the
     * press; the chip up and a waiting answer that refreshes the words
     * between press and lift (the walker's guess, 1639's refresh); and
     * the control, no chip, which always worked.
     */
    private static void aTapWorksWhileTheChipShows() {
        List<String> before = new ArrayList<String>();
        before.add("title:pages|Seiten");
        before.add("heading:Heute|Heute");
        for (int i = 0; i < 30; i++) {
            before.add("page:" + i + "|vor " + (i + 3) + " Min.");
        }
        before.add("title:rooms|Räume");
        before.add("server:valleirug|vor 19 Min. · über 8 Stationen");
        before.add("server:fablab|vor 2 Std.");
        int first = 25;
        int last = 34;
        int tap = 32;
        // An answer with a new page among the screen's rows: it waits.
        List<String> moved = new ArrayList<String>(before);
        moved.add(28, "page:new|gerade eben");
        // Its successor: the same rows, newer words for the servers.
        List<String> newer = new ArrayList<String>(moved);
        newer.set(newer.size() - 2, "server:valleirug|gerade eben · über 5 Stationen");
        for (int kind = 0; kind < 3; kind++) {
            ExploreRows<String> rows = new ExploreRows<String>(WORDS);
            rows.offer(before);
            boolean chip = kind < 2;
            if (chip) {
                check(rows.offer(moved, first, last, false) == ExploreRows.WAITING, "the chip");
                check(rows.waits(), "the chip shows");
            }
            Tap list = new Tap();
            String aimed = rows.at(tap);
            rows.press(tap);
            rows.down();
            if (kind == 1) {
                list.answer(rows, newer, first, last);
            }
            list.lift(rows, first, last);
            String hit = list.click(rows, tap);
            String which = kind == 0 ? "chip up" : kind == 1 ? "chip up, words refreshed" : "no chip";
            check(hit != null, which + ": the tap opens a row, ListView did not drop the click");
            check(WORDS.key(aimed).equals(WORDS.key(hit)), which + ": the tap opens " + aimed + ", not " + hit);
            if (chip) {
                check(rows.waits(), which + ": the chip still shows after the tap");
                check(rows.fresh() == 1, which + ": it counts the one new page: " + rows.fresh());
            }
            if (kind == 1) {
                check(rows.at(rows.size() - 2).endsWith("über 5 Stationen"),
                        "the words refreshed in place: " + rows.at(rows.size() - 2));
            }
        }
    }

    /**
     * Walk 8 of 1653, 43: scrolled, a server row said "gerade eben" at
     * the bottom, under rows "vor 3 Std.": the refresh in place (1639)
     * changes words, never places. Decided: while scrolled, or just
     * touched, it stays so; the list at rest at its very top for
     * {@link ExploreRows#QUIET_MS} takes the whole answer, in its order.
     */
    private static void atRestAtTheTopARefreshAlsoSorts() {
        List<String> before = Arrays.asList("title:pages|Seiten", "page:A|vor 3 Min.",
                "title:rooms|Räume", "server:old|vor 3 Std.", "server:kj5|vor 4 Std.");
        List<String> after = Arrays.asList("title:pages|Seiten", "page:A|vor 3 Min.",
                "title:rooms|Räume", "server:kj5|gerade eben", "server:old|vor 3 Std.");
        ExploreRows<String> rows = new ExploreRows<String>(WORDS);
        rows.offer(before);
        rows.touched(1000);
        // Scrolled to the servers: the words, in place.
        check(rows.offer(after, 2, 4, false, false, 9000) == ExploreRows.WAITING, "scrolled: waits");
        check("server:kj5|gerade eben".equals(rows.at(4)), "in place, newer words: " + rows.at(4));
        // At the top, but touched a moment ago: still in place.
        rows.touched(9500);
        check(rows.offer(after, 0, 4, false, true, 10000) == ExploreRows.WAITING, "just touched");
        check(rows.at(3).startsWith("server:old"), "still in place: " + rows.at(3));
        // At rest at the top for the quiet time: sorted.
        check(rows.offer(after, 0, 4, false, true, 9500 + ExploreRows.QUIET_MS) == ExploreRows.SHOWN,
                "at rest at the top: shown");
        check("server:kj5|gerade eben".equals(rows.at(3)), "sorted: " + rows.at(3));
        check(!rows.waits(), "no chip");
    }

    /**
     * Walk 9 of 1655, 44: servers left the list within minutes. One
     * hypothesis was 1639's refresh in place replacing the rows by the
     * newest answer's keys, so an answer naming fewer servers would drop
     * the rest. Measured here, it does not: a waiting or held answer that
     * names fewer rows keeps every row it does not name, the list its
     * length. (The cause was the library's list, which showed ten
     * shipped servers only; loquax/src/ui/explore.rs, hub_list.)
     */
    private static void aWaitingAnswerWithFewerRowsKeepsTheOthers() {
        List<String> before = Arrays.asList("title:pages|Seiten", "page:A|vor 3 Min.",
                "title:rooms|Räume", "server:whatcom|vor 6 Min.", "server:vallei|vor 10 Min.",
                "server:old|vor 5 Std.");
        List<String> after = Arrays.asList("title:pages|Seiten", "page:new|gerade eben",
                "page:A|vor 3 Min.", "title:rooms|Räume", "server:old|vor 5 Std.");
        ExploreRows<String> rows = new ExploreRows<String>(WORDS);
        rows.offer(before);
        check(rows.offer(after, 1, 5, false) == ExploreRows.WAITING, "it would move rows: waits");
        check(rows.size() == before.size(), "every row stays: " + rows.size());
        check("server:whatcom|vor 6 Min.".equals(rows.at(3)), "Whatcom stays: " + rows.at(3));
        check("server:vallei|vor 10 Min.".equals(rows.at(4)), "ValleiRug stays: " + rows.at(4));
        ExploreRows<String> held = new ExploreRows<String>(WORDS);
        held.offer(before);
        held.down();
        check(held.offer(after, 1, 5, false) == ExploreRows.HELD, "held");
        check(held.size() == before.size(), "held, every row stays: " + held.size());
        check(held.at(3).startsWith("server:whatcom"), "held, Whatcom stays: " + held.at(3));
    }

    /**
     * Walk 10 of 1659, 50: the chip said "8 neue Seiten", the reader opened
     * rns.moscow and came back; the list had applied the waiting order,
     * the chip was gone and other rows stood where rns.moscow had stood
     * (three times). Coming back is no verb of the reader's on the list:
     * the rows stay, the chip stays, and only the chip or the top at rest
     * shows what waits. The control: the pane before 1659 offered the
     * answer as its own verb's ({@code force}), which moved the rows.
     */
    private static void backFromAPageKeepsTheRowsAndTheChip() {
        List<String> before = explore(30);
        List<String> after = new ArrayList<String>(before);
        for (int i = 0; i < 8; i++) {
            after.add(12, "page:new" + i);
        }
        int first = 10;
        int last = 18;
        long now = 100_000;
        for (boolean control : new boolean[] {false, true}) {
            ExploreRows<String> rows = new ExploreRows<String>(KEYS);
            rows.offer(before);
            rows.touched(now - 60_000);
            check(rows.offer(after, first, last, false, false, now) == ExploreRows.WAITING,
                    "new pages on screen wait");
            String aimed = rows.at(first + 2);
            // The page opened, read, Back: the same answer, or a newer one.
            int said = control ? rows.offer(after, first, last, true, false, now + 30_000)
                    : rows.back(after, first, last, false, now + 30_000);
            if (control) {
                check(said == ExploreRows.SHOWN, "the control applies it");
                check(!aimed.equals(rows.at(first + 2)), "the control moves the row: " + rows.at(first + 2));
                check(!rows.waits(), "the control's chip is gone");
                continue;
            }
            check(said == ExploreRows.WAITING, "back: it still waits: " + said);
            check(aimed.equals(rows.at(first + 2)), "back: the row stays: " + rows.at(first + 2));
            check(rows.waits() && rows.fresh() == 8, "back: the chip counts eight: " + rows.fresh());
            check(rows.apply(), "the chip shows them");
            check(rows.size() == after.size(), "all of them: " + rows.size());
        }
        // The top at rest still takes it at once.
        ExploreRows<String> top = new ExploreRows<String>(KEYS);
        top.offer(before);
        top.touched(now - 60_000);
        check(top.offer(after, 0, 14, false, false, now) == ExploreRows.WAITING, "waits");
        check(top.back(after, 0, 14, true, now) == ExploreRows.SHOWN, "back at the top at rest: shown");
    }

    /**
     * Walk 10 of 1659, 51: a tap on "Räume öffnen", the section's row
     * under 100 pages, flashed and opened nothing, while the chip came.
     * ListView takes the click at the lift and runs it a moment later
     * (AOSP {@code onTouchUp} posts it after the pressed state's
     * duration); a {@code notifyDataSetChanged} before it ran sets
     * {@code mDataChanged}, and {@code PerformClick} drops the click. The
     * shape of 1653 with the answer that shows at once: pages heard above
     * the screen land while the finger is down (offered at the lift) or
     * between the lift and the click (the answer to the lift's own
     * {@code touch} verb), anchored, so the row stays at its pixel and
     * yet the adapter is told. The answer that waits (the order chip) was
     * no harm already. While a click is owed, an answer waits for it and
     * is offered once the click ran.
     */
    private static void aTapOnTheSectionRowSurvivesAnAnswerBeforeItsClick() {
        List<String> before = new ArrayList<String>();
        before.add("title:pages");
        before.add("heading:Heute");
        for (int i = 0; i < 100; i++) {
            before.add("page:" + i);
        }
        before.add("title:rooms");
        before.add("note:joined_empty");
        before.add("open:rooms");
        before.add("heading:servers");
        before.add("server:a");
        before.add("server:b");
        before.add("server:c");
        int open = before.indexOf("open:rooms");
        int first = open - 4;
        int last = open + 5;
        // Pages heard above the screen: shown at once, anchored.
        List<String> above = new ArrayList<String>(before);
        above.add(2, "page:fresh");
        // The servers reordered on screen: it waits, the order chip.
        List<String> reordered = new ArrayList<String>(before);
        Collections.swap(reordered, reordered.size() - 1, reordered.size() - 3);
        String[] cases = {"pages above, while down", "pages above, before the click",
            "order on screen, while down"};
        for (int kind = 0; kind < 3; kind++) {
            ExploreRows<String> rows = new ExploreRows<String>(KEYS);
            rows.offer(before);
            Tap list = new Tap();
            rows.press(open);
            rows.down();
            if (kind != 1) {
                list.answer(rows, kind == 0 ? above : reordered, first, last);
            }
            list.lift(rows, first, last);
            if (kind == 1) {
                list.answer(rows, above, first, last);
            }
            String hit = list.click(rows, open);
            check(hit != null, cases[kind] + ": ListView did not drop the click");
            check("open:rooms".equals(hit), cases[kind] + ": the tap opens the rooms: " + hit);
            // Once the click ran, what came is offered as the screen stands.
            int shift = kind == 2 ? 0 : 1;
            int said = rows.settle(first + shift, last + shift);
            if (kind == 2) {
                check(rows.waits() && rows.fresh() == 0, "the order chip: " + rows.fresh());
            } else {
                check(said == ExploreRows.SHOWN, cases[kind] + ": offered after the click: " + said);
                check(rows.size() == above.size(), cases[kind] + ": the new page is in: " + rows.size());
            }
        }
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
