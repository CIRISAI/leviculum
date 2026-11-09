package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * The rules of the Explore pane that need no screen (1618, the walk of
 * 17024c3a), so the host runs them ({@code tests/java/ExploreRowsTest.java}):
 * the search field that never loses a character, and the rows that never
 * move under a finger.
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
 * <p>The rows: an answer that arrives while a finger is down waits until
 * it lifts, so the row under the finger is the row the tap opens.
 *
 * @param <T> one row as the pane draws it
 */
final class ExploreRows<T> {
    private List<T> shown = Collections.emptyList();
    private List<T> waiting;
    private boolean down;

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

    /** New rows from the library: shown now (true), or held while a finger is down. */
    boolean offer(List<T> rows) {
        if (down) {
            waiting = new ArrayList<T>(rows);
            return false;
        }
        shown = new ArrayList<T>(rows);
        waiting = null;
        return true;
    }

    /** A finger came down on the list. */
    void down() {
        down = true;
    }

    /** The finger lifted: true when rows that waited are shown now. */
    boolean up() {
        down = false;
        if (waiting == null) {
            return false;
        }
        shown = waiting;
        waiting = null;
        return true;
    }

    boolean held() {
        return down;
    }

    int size() {
        return shown.size();
    }

    T at(int position) {
        return shown.get(position);
    }
}
