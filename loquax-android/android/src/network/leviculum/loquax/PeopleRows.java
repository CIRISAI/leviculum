package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The rows of a new conversation's list (1448, UI walk 1 finding 17). The
 * screen lists the people as a messenger's contact list, avatar and name; the
 * group picker is the same list with check marks, not a dialog of names. The
 * library says who may be picked and who is (the group screen's rows: peer,
 * name, picked) and how each person looks (the people's rows: peer, name,
 * initials, photo); this joins the two, in the people's order, each picker row
 * with its look. A candidate without a row of its own keeps its name alone.
 * Pure, so the host runs it (tests/java/PeopleRowsTest.java).
 */
final class PeopleRows {
    private PeopleRows() {}

    /** The picker's rows: `people` that are `candidates`, with "picked" set, then the rest of them. */
    static List<Map<String, Object>> pickable(List<Map<String, Object>> people,
            List<Map<String, Object>> candidates) {
        Map<String, Map<String, Object>> byPeer = new LinkedHashMap<>();
        for (Map<String, Object> candidate : candidates) {
            byPeer.put(str(candidate, "peer"), candidate);
        }
        List<Map<String, Object>> rows = new ArrayList<>();
        for (Map<String, Object> person : people) {
            Map<String, Object> candidate = byPeer.remove(str(person, "peer"));
            if (candidate == null) {
                continue;
            }
            Map<String, Object> row = new HashMap<>(person);
            row.put("picked", Boolean.TRUE.equals(candidate.get("picked")));
            rows.add(row);
        }
        for (Map<String, Object> candidate : byPeer.values()) {
            Map<String, Object> row = new HashMap<>(candidate);
            row.put("picked", Boolean.TRUE.equals(candidate.get("picked")));
            rows.add(row);
        }
        return rows;
    }

    /** How many rows are picked: the button says so. */
    static int picked(List<Map<String, Object>> rows) {
        int count = 0;
        for (Map<String, Object> row : rows) {
            if (Boolean.TRUE.equals(row.get("picked"))) {
                count++;
            }
        }
        return count;
    }

    private static String str(Map<String, Object> row, String key) {
        Object value = row.get(key);
        return value == null ? "" : value.toString();
    }
}
