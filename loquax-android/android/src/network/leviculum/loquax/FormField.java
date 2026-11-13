package network.leviculum.loquax;

import java.util.List;
import java.util.Map;

/**
 * A page's text field as its editor says it (1667, walk 11, finding 59):
 * the dialog's title was the field's raw name ("q") and it carried no
 * hint. The hint is what the page itself writes before the field on its
 * line ("Search: "), the words a reader sees beside the grey bar. No
 * Android here, so the host runs it (tests/java/FormFieldTest.java).
 */
final class FormField {
    private FormField() {
    }

    /**
     * The text the page shows before the run of `node` on the line that
     * holds it, trimmed, or "" where the field opens its line, the page
     * writes only blanks there, or no line holds it.
     */
    @SuppressWarnings("unchecked")
    static String hint(List<Map<String, Object>> lines, String node) {
        if (lines == null || node == null || node.isEmpty()) {
            return "";
        }
        for (Map<String, Object> line : lines) {
            Object runs = line.get("runs");
            if (!(runs instanceof List)) {
                continue;
            }
            StringBuilder before = new StringBuilder();
            for (Object item : (List<Object>) runs) {
                if (!(item instanceof Map)) {
                    continue;
                }
                Map<String, Object> run = (Map<String, Object>) item;
                if (node.equals(run.get("node"))) {
                    return before.toString().trim();
                }
                Object text = run.get("text");
                if (text instanceof String) {
                    before.append((String) text);
                }
            }
        }
        return "";
    }
}
