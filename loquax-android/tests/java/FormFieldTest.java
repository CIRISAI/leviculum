package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The form field's hint (1667, walk 11, finding 59), on the host, run by
 * tests/fling.rs: the words the page writes before the field on its line,
 * nothing where it writes none. Control: a field on a line of its own, and
 * a node no line holds, give no hint.
 */
final class FormFieldTest {
    private static int checks;

    public static void main(String[] args) {
        List<Map<String, Object>> lines = new ArrayList<>();
        lines.add(line(run("STRUDEL - Search Engine", "")));
        lines.add(line(run("  Search: ", ""), run("________________", "field_1"), run(" ", ""),
                run("[Go]", "link_3")));
        lines.add(line(run("________", "field_2")));
        lines.add(line(run("   ", ""), run("________", "field_3")));

        check("Search:".equals(FormField.hint(lines, "field_1")), FormField.hint(lines, "field_1"));
        check(FormField.hint(lines, "field_2").isEmpty(), "a field opening its line has no hint");
        check(FormField.hint(lines, "field_3").isEmpty(), "blanks before a field are no hint");
        check(FormField.hint(lines, "field_9").isEmpty(), "control: a node no line holds");
        check(FormField.hint(null, "field_1").isEmpty(), "no lines");
        System.out.println(checks + " checks passed");
    }

    private static Map<String, Object> run(String text, String node) {
        Map<String, Object> run = new HashMap<>();
        run.put("text", text);
        run.put("node", node);
        return run;
    }

    @SafeVarargs
    private static Map<String, Object> line(Map<String, Object>... runs) {
        Map<String, Object> line = new HashMap<>();
        line.put("runs", new ArrayList<Object>(Arrays.asList((Object[]) runs)));
        return line;
    }

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }
}
