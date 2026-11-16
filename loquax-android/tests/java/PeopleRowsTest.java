package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The new conversation's picker rows (1448, walk finding 17), on the host, run
 * by tests/fling.rs: the group picker is the people's list with check marks,
 * in the people's order and with their look; a person who is no candidate (a
 * member already) is not in it; a candidate without a look keeps its name.
 * Control: the dialog before 1448 listed the candidates' names alone.
 */
final class PeopleRowsTest {
    private static int checks;

    public static void main(String[] args) {
        List<Map<String, Object>> people = new ArrayList<>();
        people.add(row("aa", "Anna", "AN", null));
        people.add(row("bb", "Ben", "BE", "/photos/bb"));
        people.add(row("cc", "Cleo", "CL", null));
        List<Map<String, Object>> candidates = new ArrayList<>();
        candidates.add(candidate("cc", "Cleo", true));
        candidates.add(candidate("aa", "Anna", false));
        candidates.add(candidate("dd", "Dora", false));

        List<Map<String, Object>> rows = PeopleRows.pickable(people, candidates);
        check(rows.size() == 3, "three rows: " + rows);
        check("aa".equals(rows.get(0).get("peer")) && "cc".equals(rows.get(1).get("peer")),
                "the people's order: " + rows);
        check("CL".equals(rows.get(1).get("initials")), "with the look");
        check(Boolean.TRUE.equals(rows.get(1).get("picked")), "Cleo picked");
        check(Boolean.FALSE.equals(rows.get(0).get("picked")), "Anna not");
        check("Dora".equals(rows.get(2).get("name")) && rows.get(2).get("initials") == null,
                "a candidate without a look keeps its name");
        for (Map<String, Object> row : rows) {
            check(!"bb".equals(row.get("peer")), "Ben is no candidate");
        }
        check(PeopleRows.picked(rows) == 1, "one picked");
        // Control: the dialog's rows before 1448, names alone.
        check(before(candidates).get(1).get("initials") == null,
                "the control has no look to show");
        System.out.println(checks + " checks passed");
    }

    private static List<Map<String, Object>> before(List<Map<String, Object>> candidates) {
        return candidates;
    }

    private static Map<String, Object> row(String peer, String name, String initials, String photo) {
        Map<String, Object> row = new HashMap<>();
        row.put("peer", peer);
        row.put("name", name);
        row.put("initials", initials);
        if (photo != null) {
            row.put("photo", photo);
        }
        return row;
    }

    private static Map<String, Object> candidate(String peer, String name, boolean picked) {
        Map<String, Object> row = new HashMap<>();
        row.put("peer", peer);
        row.put("name", name);
        row.put("picked", picked);
        return row;
    }

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }
}
