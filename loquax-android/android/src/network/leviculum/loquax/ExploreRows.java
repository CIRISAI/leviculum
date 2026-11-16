package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * The rules of the Explore pane that need no screen (1618, the walk of
 * 17024c3a; 1621, the walk of 02e6984a), so the host runs them
 * ({@code tests/java/ExploreRowsTest.java}): the search field that never
 * loses a character, and the rows that never move between looking and
 * tapping.
 *
 * <p>The field is the source of truth. Each change goes to the library as
 * a {@code typed} verb, and the library's answer carries the query back;
 * an answer computed for an older verb holds fewer characters than the
 * field. The rule before 1618 wrote any answer that differed from the
 * field into it, and 32 characters typed fast arrived as 22. Now an answer
 * is written only while no {@code typed} verb is on its way: then the
 * library has every character, and an answer that still differs is the
 * library's own word (a field it cleared).
 *
 * <p>The rows (1621, as the chat apps do it): what is on screen never
 * moves on its own. An answer is shown at once only when every row on
 * screen keeps its neighbours in it; the row that stood first on screen
 * is then put back at its pixel ({@link #anchor}, the pane's
 * {@code setSelectionFromTop}), so rows that arrive above the screen
 * slide in unseen. An answer that would move a row on screen, new pages
 * landing at the top of "Today" while the reader aims, waits, and a chip
 * says how many new pages wait ({@link #fresh}); a tap on it shows them
 * ({@link #apply}). The reader's own verbs (the tab, a search, the order,
 * a star) show their answer at once. A finger on the list holds every
 * answer until it lifts (1618), and the lift offers it under the same
 * rule.
 *
 * <p>The top at rest (1629, walk 3: after a cold start one page stood
 * there and the chip counted to seven in 30 s while nobody touched
 * anything): while the list stands at its very top and the reader has not
 * touched or scrolled it for {@link #QUIET_MS}, an answer shows at once,
 * the list kept at its top. The chip belongs to a scrolled list, a list
 * just touched, or a finger on it.
 *
 * <p>A tap opens the row it hit (1632, walk 4: a tap on a page opened the
 * row above it): {@link #press} keeps the row under the finger and
 * {@link #hit} hands it to the click, whatever the list became between.
 *
 * @param <T> one row as the pane draws it
 */
final class ExploreRows<T> {
    /** What the rule reads of a row. */
    interface Keys<T> {
        /** The row's stable key. */
        String key(T row);

        /** Whether the row counts as a new page for the chip. */
        boolean counts(T row);
    }

    /** {@link #offer}: the answer is on screen now. */
    static final int SHOWN = 0;
    /** {@link #offer}: a finger is down; it waits for the lift. */
    static final int HELD = 1;
    /** {@link #offer}: it would move a row on screen; it waits behind the chip. */
    static final int WAITING = 2;

    /**
     * Whether the pane tells the adapter after {@code said}
     * ({@code notifyDataSetChanged}): only when the rows changed. A
     * {@link #WAITING} answer changes words in place, which the pane
     * draws into the rows on screen as they stand (1653, walk 8, 39: the
     * notification between a press and ListView's {@code onTouchUp} sets
     * its {@code mDataChanged}, and ListView drops the click; with the
     * chip up every tap opened nothing).
     */
    static boolean notifies(int said) {
        return said == SHOWN;
    }

    /** How long the list must rest untouched at its top before an answer shows there at once. */
    static final long QUIET_MS = 3000;

    private final Keys<T> keys;
    private List<T> shown = Collections.emptyList();
    private List<T> waiting;
    private boolean down;
    /** An answer came while the finger was down. */
    private boolean arrived;
    private int anchor = -1;
    /** The row the finger came down on, until its click or the gesture's end. */
    private T pressed;
    /**
     * The finger lifted off {@link #pressed} and ListView's click is still
     * to run (1659, walk 10, 51): answers wait for it ({@link #settle}).
     */
    private boolean owed;
    /** When the reader last touched or scrolled the list; never, at first. */
    private long touched = Long.MIN_VALUE / 2;

    ExploreRows(Keys<T> keys) {
        this.keys = keys;
    }

    /** Whether the field takes the library's {@code answer}: no verb in flight, and it differs. */
    static boolean echo(String answer, int inFlight, String field) {
        return inFlight == 0 && !answer.equals(field);
    }

    /** The rule before 1618, the test's control. */
    static boolean echoBefore(String answer, String field) {
        return !answer.equals(field);
    }

    /** A stable id for a row's key: the list keeps a row's place and scroll by it. */
    static long id(String key) {
        long hash = 0xcbf29ce484222325L;
        for (int i = 0; i < key.length(); i++) {
            hash ^= key.charAt(i);
            hash *= 0x100000001b3L;
        }
        return hash;
    }

    /** New rows with nothing known of the screen: shown, unless a finger is down. */
    boolean offer(List<T> rows) {
        return offer(rows, -1, -1, true) == SHOWN;
    }

    /**
     * New rows from the library while the screen shows the rows
     * {@code first} to {@code last} of the ones shown ({@code -1} when it
     * shows none yet); {@code force} for the answer to the reader's own
     * verb. {@link #SHOWN}, {@link #HELD} or {@link #WAITING}.
     */
    int offer(List<T> rows, int first, int last, boolean force) {
        return offer(rows, first, last, force, false, 0);
    }

    /**
     * {@link #offer(List, int, int, boolean)} with the screen's top:
     * {@code atTop} when the list stands at its very top, {@code now} the
     * clock {@link #touched} reads.
     */
    int offer(List<T> rows, int first, int last, boolean force, boolean atTop, long now) {
        List<T> copy = new ArrayList<T>(rows);
        // A click owed waits nothing out: ListView runs it a moment after
        // the lift and drops it when the adapter was told meanwhile (1659,
        // walk 10, 51). The answer waits, as under the finger, and is
        // offered once the click ran.
        if (down || owed) {
            refresh(copy);
            waiting = copy;
            arrived = true;
            return HELD;
        }
        if (atTop && now - touched >= QUIET_MS) {
            show(copy);
            anchor = 0;
            return SHOWN;
        }
        anchor = -1;
        if (force || shown.isEmpty() || first < 0) {
            show(copy);
            return SHOWN;
        }
        int at = keeps(copy, first, last);
        if (at >= 0) {
            anchor = at;
            show(copy);
            return SHOWN;
        }
        refresh(copy);
        waiting = copy;
        return WAITING;
    }

    /**
     * An answer that waits still gives every row it shares with the list
     * its words, in place (1639, walk 5 new 26: a server row said "vor 19
     * Min. · über 8 Stationen" while its sheet, drawn from the newest
     * answer, said "gerade eben · über 5 Stationen"). Rows keep their
     * places and the list its length; only what a row says is newer.
     */
    private void refresh(List<T> rows) {
        java.util.Map<String, T> by = new java.util.HashMap<String, T>();
        for (T row : rows) {
            by.put(keys.key(row), row);
        }
        List<T> now = new ArrayList<T>(shown.size());
        for (T row : shown) {
            T newer = by.get(keys.key(row));
            now.add(newer != null ? newer : row);
        }
        shown = now;
    }

    /**
     * Where the screen's rows stand in {@code rows}: the new position of
     * the first, when every row from {@code first} to {@code last} keeps
     * its place after it; -1 when one would move.
     */
    private int keeps(List<T> rows, int first, int last) {
        int end = Math.min(last, shown.size() - 1);
        if (first > end) {
            return 0;
        }
        String top = keys.key(shown.get(first));
        int at = -1;
        for (int i = 0; i < rows.size(); i++) {
            if (keys.key(rows.get(i)).equals(top)) {
                at = i;
                break;
            }
        }
        if (at < 0) {
            return -1;
        }
        for (int i = first; i <= end; i++) {
            int j = at + i - first;
            if (j >= rows.size() || !keys.key(rows.get(j)).equals(keys.key(shown.get(i)))) {
                return -1;
            }
        }
        return at;
    }

    private void show(List<T> rows) {
        shown = rows;
        waiting = null;
    }

    /**
     * The {@code y} for {@code setSelectionFromTop} that puts a row back
     * where {@code getTop()} read it, in a list with a top band of
     * {@code padding} pixels (1639, walk 5 new 21). {@code getTop()}
     * counts from the list's edge, the {@code y} from inside the band:
     * handed back as read, every anchored answer moved the list one band
     * down, 20 dp, and an untouched list crept down the screen.
     */
    static int fromTop(int rowTop, int padding) {
        return rowTop - padding;
    }

    /** The row that stood first on screen, where it stands now; -1 to keep the scroll as it is. */
    int anchor() {
        return anchor;
    }

    /** The reader touched or scrolled the list at {@code now}. */
    void touched(long now) {
        touched = now;
    }

    /** A finger came down on the list. */
    void down() {
        down = true;
        arrived = false;
    }

    /**
     * The finger lifted: an answer that came while it was down is offered
     * as the screen stands, {@link #SHOWN} or {@link #WAITING};
     * {@link #HELD} when none came. What already waited behind the chip
     * before the press is not offered again (1653, walk 8, 39): it waited
     * as the screen stood, and offering it again told the adapter and
     * cost the tap its click.
     */
    int up(int first, int last) {
        down = false;
        owed = pressed != null;
        if (waiting == null || !arrived || owed) {
            return HELD;
        }
        arrived = false;
        return offer(waiting, first, last, false);
    }

    /**
     * The click owed ran, or will not come (a scroll, a long press, a row
     * that takes no click; the pane's timer after the pressed state's
     * duration): what came meanwhile is offered as the screen stands,
     * {@link #SHOWN} or {@link #WAITING}; {@link #HELD} when none came or
     * a finger is down again (1659, walk 10, 51).
     */
    int settle(int first, int last) {
        owed = false;
        pressed = null;
        if (down || waiting == null || !arrived) {
            return HELD;
        }
        arrived = false;
        return offer(waiting, first, last, false);
    }

    /** The lift of the 1618 rule, the screen unknown: what waited shows. */
    boolean up() {
        down = false;
        arrived = false;
        if (waiting == null) {
            return false;
        }
        show(waiting);
        anchor = -1;
        return true;
    }

    boolean held() {
        return down;
    }

    /** Whether an answer waits behind the chip. */
    boolean waits() {
        return waiting != null && !down;
    }

    /** How many new pages wait: the counted rows of the waiting answer not on the list now. */
    int fresh() {
        if (waiting == null) {
            return 0;
        }
        Set<String> now = new HashSet<String>();
        for (T row : shown) {
            now.add(keys.key(row));
        }
        int n = 0;
        for (T row : waiting) {
            if (keys.counts(row) && !now.contains(keys.key(row))) {
                n++;
            }
        }
        return n;
    }

    /** The chip tapped: what waited is shown, from the top. */
    boolean apply() {
        if (waiting == null) {
            return false;
        }
        show(waiting);
        anchor = -1;
        return true;
    }

    /**
     * The reader came back from a page or the rooms it opened (1659, walk
     * 10, 50: the waiting order applied on Back, the chip went, and other
     * rows stood where the page just read had stood). Coming back is no
     * verb on the list: the answer is offered as the screen stands, so
     * what waited still waits behind the chip; only the chip, or the top
     * at rest, shows it.
     */
    int back(List<T> rows, int first, int last, boolean atTop, long now) {
        return offer(rows, first, last, false, atTop, now);
    }


    int size() {
        return shown.size();
    }

    /**
     * The finger came down on {@code position} of the rows shown (1632,
     * walk 4): the row it hit is kept, by itself, not by its position.
     * ListView takes the position at the press and clicks it a moment
     * after the lift; an answer that lands in between and shows at once
     * (pages above the screen, anchored) makes that position name another
     * row. A position outside the rows keeps nothing.
     */
    void press(int position) {
        pressed = position >= 0 && position < shown.size() ? shown.get(position) : null;
    }

    /** The gesture became a scroll or was cancelled: no click follows it. */
    void release() {
        pressed = null;
        owed = false;
    }

    /**
     * The row a click at {@code position} opens: the row the finger came
     * down on, when a press came before it; else the row at the position
     * (a key, accessibility). A press is used once.
     */
    T hit(int position) {
        T row = pressed;
        pressed = null;
        owed = false;
        return row != null ? row : shown.get(position);
    }

    /** Where the row with {@code key} stands among the rows shown; -1 when it is not (1659, 54). */
    int find(String key) {
        for (int i = 0; i < shown.size(); i++) {
            if (keys.key(shown.get(i)).equals(key)) {
                return i;
            }
        }
        return -1;
    }

    T at(int position) {
        return shown.get(position);
    }
}
