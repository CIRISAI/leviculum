package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * When nothing changes, nothing is drawn (1455), on the host, run by
 * tests/fling.rs: the phone left alone draws nothing. A map with a live
 * pin, its frame shown and then nothing for five seconds after the pulse
 * ended, asks no frame; the chat list's 30-second tick, the library's
 * answer unchanged, draws nothing in two minutes. The controls: the
 * pulse's own seconds ask frames, the rule before 1455 (a frame every
 * 66 ms while a live pin is shown) asks them all along, and a row whose
 * age turned from "now" into "1 min" is drawn.
 */
final class QuietTest {
    private static int checks;

    /** The map's pulse cycle and its frame interval (MapView). */
    static final long PULSE_MS = 2000;
    static final long PULSE_FRAME_MS = 66;

    @SuppressWarnings("unchecked")
    public static void main(String[] args) {
        // The map: a frame with a live pin at 0, then no event at all.
        int during = frames(0, 0, PULSE_MS * Quiet.PULSE_CYCLES, true);
        check(during > 50, "the pulse's own seconds ask frames: " + during);
        long quietFrom = PULSE_MS * Quiet.PULSE_CYCLES;
        int idle = frames(0, quietFrom, quietFrom + 5000, true);
        check(idle == 0, "five idle seconds after the pulse ask " + idle + " frames");
        int before = frames(0, quietFrom, quietFrom + 5000, false);
        check(before >= 70, "the rule before 1455 asks " + before + " frames in five seconds");

        // The chat list: a tick every 30 s for two minutes.
        Map<String, Object> shown = answer("now");
        int draws = 0;
        for (int tick = 0; tick < 4; tick++) {
            Map<String, Object> asked = answer("now");
            if (!Quiet.same(shown, asked)) {
                draws++;
                shown = asked;
            }
        }
        check(draws == 0, "two quiet minutes drew the list " + draws + " times");
        check(!Quiet.same(shown, answer("1 min")), "a row's age that moved is drawn");
        Map<String, Object> avatar = answer("now");
        ((Map<String, Object>) ((List<?>) avatar.get("conversations")).get(0))
                .put("photo", new byte[] {1, 2, 4});
        check(!Quiet.same(shown, avatar), "a new picture is drawn");
        check(Quiet.same(avatar, answer("now", new byte[] {1, 2, 4})),
                "the same picture in another array is the same");
        check(!Quiet.same(null, shown) && Quiet.same(null, null), "nothing on screen yet");

        System.out.println(checks + " checks passed");
    }

    /**
     * The frames MapView asks between {@code from} and {@code to}, its
     * frame shown at {@code since}: each drawn frame of a pulsing ring
     * asks the next PULSE_FRAME_MS later. {@code quiet} the 1455 rule,
     * else every frame pulses.
     */
    private static int frames(long since, long from, long to, boolean quiet) {
        int asked = 0;
        for (long now = since; now < to; now += PULSE_FRAME_MS) {
            boolean pulsing = !quiet || Quiet.pulsing(since, now, PULSE_MS);
            if (!pulsing) {
                break;
            }
            if (now >= from) {
                asked++;
            }
        }
        return asked;
    }

    /** The library's chat list answer with one row aged {@code age}. */
    private static Map<String, Object> answer(String age) {
        return answer(age, null);
    }

    private static Map<String, Object> answer(String age, byte[] photo) {
        Map<String, Object> row = new HashMap<>();
        row.put("peer", "0c0c0c0c");
        row.put("name", "Anna");
        row.put("age", age);
        row.put("unread", 2L);
        if (photo != null) {
            row.put("photo", photo);
        }
        List<Object> rows = new ArrayList<>();
        rows.add(row);
        Map<String, Object> answer = new HashMap<>();
        answer.put("conversations", rows);
        answer.put("home_link", true);
        return answer;
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
