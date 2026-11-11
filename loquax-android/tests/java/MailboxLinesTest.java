package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The mailbox page's rule (1647, walk 6, finding 28), on the host, run by
 * tests/fling.rs.
 *
 * Twelve answers, one every five seconds, "connected for" and the last
 * contact moving: every one fits the page on screen and is written into
 * its lines, so no view is removed and the scroll position stays. The
 * control compares words instead of the shape: nearly every answer
 * differs, which under the rule before 1647 (every answer drawn into an
 * emptied page) was a jump to the top each time. An answer of another
 * shape (the mailbox's own word arrives) does not fit and is drawn whole.
 */
final class MailboxLinesTest {
    private static int checks;

    /** A line on screen: what it says, and how often it was written. */
    private static final class Line implements MailboxLines.Slot {
        String label;
        String value;
        int writes;

        @Override
        public void said(String label, String value) {
            this.label = label;
            this.value = value;
            writes++;
        }
    }

    public static void main(String[] args) {
        MailboxLines lines = new MailboxLines();
        check(!lines.fits(page(0, false)), "nothing on screen: the first answer is drawn whole");
        List<Line> onScreen = draw(lines, page(0, false));
        int rebuilds = 1;
        for (int ask = 1; ask <= 12; ask++) {
            Map<String, Object> answer = page(ask * 5, false);
            if (lines.fits(answer)) {
                lines.said(answer);
            } else {
                onScreen = draw(lines, answer);
                rebuilds++;
            }
        }
        check(rebuilds == 1, "twelve asks, no rebuild: " + rebuilds);
        check("Dieses Gerät verbunden seit 1 Min.".equals(onScreen.get(2).label), onScreen.get(2).label);
        check(onScreen.get(2).writes == 12, "written in place twelve times");
        check("Meinen Code zeigen".equals(onScreen.get(onScreen.size() - 1).label),
                "the line below the fold is the same view");

        // The control: a rule that compares the words, not the shape,
        // rebuilds on every ask, as "connected for" moves each time.
        int control = 0;
        Map<String, Object> last = page(0, false);
        for (int ask = 1; ask <= 12; ask++) {
            Map<String, Object> answer = page(ask * 5, false);
            if (!answer.equals(last)) {
                control++;
            }
            last = answer;
        }
        check(control >= 10, "the words move on nearly every ask: " + control);

        // The mailbox's own word arrives: another shape, drawn whole.
        Map<String, Object> told = page(65, true);
        check(!lines.fits(told), "a device more is another shape");
        onScreen = draw(lines, told);
        check(lines.fits(page(70, true)), "and the next ask fits again");

        lines.clear();
        check(!lines.fits(page(75, true)), "a page that went draws whole");
        System.out.println(checks + " checks passed");
    }

    private static List<Line> draw(MailboxLines lines, Map<String, Object> answer) {
        List<Line> drawn = new ArrayList<Line>();
        List<MailboxLines.Slot> slots = new ArrayList<MailboxLines.Slot>();
        for (Map<String, Object> group : list(answer)) {
            for (Object item : (List<?>) group.get("lines")) {
                Line line = new Line();
                @SuppressWarnings("unchecked")
                Map<String, Object> map = (Map<String, Object>) item;
                line.label = (String) map.get("label");
                drawn.add(line);
                slots.add(line);
            }
        }
        lines.drawn(answer, slots);
        return drawn;
    }

    @SuppressWarnings("unchecked")
    private static List<Map<String, Object>> list(Map<String, Object> answer) {
        return (List<Map<String, Object>>) answer.get("groups");
    }

    /** The page `secs` after the link came up, with the M9 once `told`. */
    private static Map<String, Object> page(int secs, boolean told) {
        List<Object> which = new ArrayList<Object>();
        which.add(line("Name", "Lew Palm", null, null, false));
        which.add(line("Adresse", "5a5a", "5a5a", null, false));
        List<Object> reach = new ArrayList<Object>();
        reach.add(line(secs < 60 ? "Dieses Gerät verbunden seit weniger als einer Minute"
                : "Dieses Gerät verbunden seit " + secs / 60 + " Min.", null, null, null, false));
        reach.add(line("Letzter Kontakt", "vor " + secs + " Sek.", null, null, false));
        List<Object> devices = new ArrayList<Object>();
        devices.add(line("Fairphone", "dieses Gerät", null, null, false));
        if (told) {
            devices.add(line("M9", null, null, null, false));
        }
        devices.add(line("Ein Gerät wird am Postfach selbst hinzugefügt oder entfernt.",
                null, null, null, true));
        devices.add(line("Meinen Code zeigen", null, null, "profile", false));
        List<Object> groups = new ArrayList<Object>();
        groups.add(group("Welches Postfach", which));
        groups.add(group("Verbindung", reach));
        groups.add(group("Geräte dieses Postfachs", devices));
        Map<String, Object> answer = new HashMap<String, Object>();
        answer.put("groups", groups);
        return answer;
    }

    private static Map<String, Object> group(String title, List<Object> lines) {
        Map<String, Object> group = new HashMap<String, Object>();
        group.put("title", title);
        group.put("lines", lines);
        return group;
    }

    private static Map<String, Object> line(String label, String value, String copy,
            String action, boolean note) {
        Map<String, Object> line = new HashMap<String, Object>();
        line.put("label", label);
        line.put("value", value);
        line.put("copy", copy);
        line.put("action", action);
        line.put("note", note);
        return line;
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
