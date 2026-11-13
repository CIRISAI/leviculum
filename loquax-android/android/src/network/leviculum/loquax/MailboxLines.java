package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;

/**
 * The mailbox page's rule that needs no screen (1647, walk 6, finding 28),
 * so the host runs it ({@code tests/java/MailboxLinesTest.java}): the page
 * holds still while it is asked again.
 *
 * <p>The page asks the library again every five seconds, so "connected
 * for" and the last contact move. Before 1647 each ask removed every view
 * and drew the answer into an empty page when it came: the scroll view
 * lost its height in between and stood at the top again, and "Meinen Code
 * zeigen" below the fold could not be reached (walk 6: scrolled at 1.5 s,
 * at the top at 2.6 s).
 *
 * <p>Now an answer of the same {@link #shape} as the page on screen is
 * written into the lines as they stand ({@link #said}): no view comes or
 * goes, and the scroll position is not touched. An answer of another
 * shape (the mailbox's own word arrived, a device came) is drawn whole,
 * at once, and the activity puts the scroll position back.
 */
final class MailboxLines {
    /** A line as it stands on screen: its label and the value under it. */
    interface Slot {
        void said(String label, String value);
    }

    private List<String> shape = Collections.emptyList();
    private final List<Slot> slots = new ArrayList<Slot>();

    /**
     * What an answer's page is made of, one word per group and line: the
     * group's title, and per line its kind (a note, a line with a copy
     * button, one with an action, a plain one) and whether it has a value.
     * Two answers of one shape differ only in words.
     */
    static List<String> shape(Map<String, Object> answer) {
        List<String> shape = new ArrayList<String>();
        for (Map<String, Object> group : list(answer, "groups")) {
            shape.add("group " + str(group, "title"));
            for (Map<String, Object> line : list(group, "lines")) {
                String kind;
                if (Boolean.TRUE.equals(line.get("note"))) {
                    kind = "note";
                } else if (!str(line, "copy").isEmpty()) {
                    kind = "copy";
                } else if (!str(line, "action").isEmpty()) {
                    kind = "action " + str(line, "action");
                } else {
                    kind = "line";
                }
                shape.add(kind + (str(line, "value").isEmpty() ? "" : " value"));
            }
        }
        return shape;
    }

    /** Whether `answer` can be written into the lines on screen. */
    boolean fits(Map<String, Object> answer) {
        return !slots.isEmpty() && shape.equals(shape(answer));
    }

    /** A new page drawn whole: its shape, and its lines in order. */
    void drawn(Map<String, Object> answer, List<Slot> lines) {
        shape = shape(answer);
        slots.clear();
        slots.addAll(lines);
    }

    /** Write a fitting answer's words into the lines on screen. */
    void said(Map<String, Object> answer) {
        int i = 0;
        for (Map<String, Object> group : list(answer, "groups")) {
            for (Map<String, Object> line : list(group, "lines")) {
                if (i < slots.size()) {
                    slots.get(i).said(str(line, "label"), str(line, "value"));
                }
                i++;
            }
        }
    }

    /** The page went: the next answer is drawn whole. */
    void clear() {
        shape = Collections.emptyList();
        slots.clear();
    }

    private static String str(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value == null ? "" : String.valueOf(value);
    }

    @SuppressWarnings("unchecked")
    private static List<Map<String, Object>> list(Map<String, Object> map, String key) {
        Object value = map.get(key);
        List<Map<String, Object>> out = new ArrayList<Map<String, Object>>();
        if (value instanceof List) {
            for (Object item : (List<Object>) value) {
                if (item instanceof Map) {
                    out.add((Map<String, Object>) item);
                }
            }
        }
        return out;
    }
}
