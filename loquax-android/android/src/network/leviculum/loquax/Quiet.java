package network.leviculum.loquax;

import java.util.Arrays;
import java.util.Iterator;
import java.util.List;
import java.util.Map;

/**
 * When nothing changes, nothing is drawn (1455): the rules the screens
 * ask before they draw again, plain Java so the host runs them
 * (tests/fling.rs with tests/java/QuietTest.java).
 *
 * The chat list and the chat ask the library again every 30 seconds, so
 * that "now" turns into "1 min" without a message; they draw the answer
 * only when it differs from the one on screen ({@link #same}). The map's
 * live ring pulses for a few cycles after a frame put it there and then
 * stands ({@link #pulsing}); before, it asked for a frame every 66 ms for
 * as long as a live pin was on screen.
 */
final class Quiet {
    /** How many slow cycles a live ring pulses after its frame came. */
    static final int PULSE_CYCLES = 3;

    private Quiet() {}

    /**
     * Whether a live ring still pulses at {@code now}, its frame shown at
     * {@code since}, a cycle {@code cycleMs} long: uptime milliseconds.
     */
    static boolean pulsing(long since, long now, long cycleMs) {
        return now - since < PULSE_CYCLES * cycleMs;
    }

    /**
     * Whether two answers of the library are the same: maps, lists,
     * strings, numbers, booleans and byte arrays, compared by what they
     * hold (a byte array's equals is its identity).
     */
    static boolean same(Object a, Object b) {
        if (a == b) {
            return true;
        }
        if (a == null || b == null) {
            return false;
        }
        if (a instanceof byte[] && b instanceof byte[]) {
            return Arrays.equals((byte[]) a, (byte[]) b);
        }
        if (a instanceof Map && b instanceof Map) {
            Map<?, ?> x = (Map<?, ?>) a;
            Map<?, ?> y = (Map<?, ?>) b;
            if (x.size() != y.size()) {
                return false;
            }
            for (Map.Entry<?, ?> e : x.entrySet()) {
                if (!y.containsKey(e.getKey()) || !same(e.getValue(), y.get(e.getKey()))) {
                    return false;
                }
            }
            return true;
        }
        if (a instanceof List && b instanceof List) {
            List<?> x = (List<?>) a;
            List<?> y = (List<?>) b;
            if (x.size() != y.size()) {
                return false;
            }
            Iterator<?> i = x.iterator();
            Iterator<?> j = y.iterator();
            while (i.hasNext()) {
                if (!same(i.next(), j.next())) {
                    return false;
                }
            }
            return true;
        }
        return a.equals(b);
    }
}
