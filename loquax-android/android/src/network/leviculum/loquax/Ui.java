package network.leviculum.loquax;

import android.content.Context;
import android.util.Log;
import android.view.View;
import android.widget.TextView;
import java.text.DateFormat;
import java.util.Date;
import java.util.Locale;
import java.util.Map;

/** What the screens share: times and the status line's words. */
final class Ui {
    /** The library's answer to a call without a session (src/lib.rs). */
    static final String NOT_STARTED = "not started";

    private Ui() {}

    /** A time of day, in the phone's locale: 18:34 or 6:34 PM. */
    /**
     * A place, degrees times a million, as the Maps page names a region
     * (1201): "52.52° N, 13.40° E", two decimals, in the phone's way of
     * writing them.
     */
    static String place(long latE6, long lonE6) {
        return String.format(java.util.Locale.getDefault(), "%.2f° %s, %.2f° %s",
                Math.abs(latE6) / 1e6, latE6 < 0 ? "S" : "N",
                Math.abs(lonE6) / 1e6, lonE6 < 0 ? "W" : "E");
    }

    static String clock(long unixSecs) {
        return DateFormat.getTimeInstance(DateFormat.SHORT).format(new Date(unixSecs * 1000));
    }

    /** Within the last hour, how long ago in words ("59 Min."); null otherwise. */
    static String ago(Context context, long unixSecs, long nowSecs) {
        long secs = nowSecs - unixSecs;
        if (secs < 0 || secs >= 3600) {
            return null;
        }
        if (secs < 60) {
            return context.getString(R.string.just_now);
        }
        return context.getString(R.string.minutes_ago, (int) (secs / 60));
    }

    /** How old a position is, as the terminal's header says it: now, minutes, hours, days. */
    static String age(Context context, long unixSecs, long nowSecs) {
        long secs = Math.max(0, nowSecs - unixSecs);
        if (secs < 60) {
            return context.getString(R.string.just_now);
        }
        if (secs < 3600) {
            return context.getString(R.string.minutes_ago, (int) (secs / 60));
        }
        if (secs < 86_400) {
            return context.getString(R.string.hours_ago, (int) (secs / 3600));
        }
        return context.getString(R.string.days_ago, (int) (secs / 86_400));
    }

    /**
     * The list's line under a name (1130): a position report and a
     * reaction in the phone's language, a text as it came. An older
     * library without `kind` gives its English line.
     */
    static String preview(Context context, Map<String, Object> row) {
        String kind = Core.str(row, "kind");
        String live = Core.str(row, "live");
        if ("position".equals(kind) && !live.isEmpty()) {
            // Whether the position is alive, never how many came (1209),
            // in the library's words (1217): `📍 live` or `📍 3 h ago`.
            return "\uD83D\uDCCD " + live;
        }
        String reaction = Core.str(row, "reaction");
        if ("reaction".equals(kind) && !reaction.isEmpty()) {
            return context.getString(Core.bool(row, "ours") ? R.string.you_reacted : R.string.reacted, reaction);
        }
        return Core.str(row, "last");
    }

    /**
     * The delivery of one of ours as a screen reader says it (1232):
     * `readAt` is the reader's time for "read", unix seconds.
     */
    static String state(Context context, String state, long readAt) {
        switch (state) {
            case "sending": return context.getString(R.string.state_sending);
            case "unconfirmed": return context.getString(R.string.state_unconfirmed);
            case "mailbox": return context.getString(R.string.state_mailbox);
            case "arrived": return context.getString(R.string.state_arrived);
            case "delivered": return context.getString(R.string.state_delivered);
            // 0 is a read at no known time (1506, an import's): "Read".
            case "read": return readAt > 0
                    ? context.getString(R.string.state_read, clock(readAt))
                    : context.getString(R.string.state_read_untimed);
            case "failed": return context.getString(R.string.state_failed);
            default: return state;
        }
    }

    static long nowSecs() {
        return System.currentTimeMillis() / 1000;
    }

    /** How long a line about something that happened stays before it hides. */
    static final long LINE_MS = 4000;

    /**
     * Whether the line `event` gives stays until the next one: only
     * "pulling", which its result follows. Any other hides after
     * {@link #LINE_MS}, so the line never says an old result.
     */
    static boolean stays(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        String state = Core.str(event, "state");
        return ("sync".equals(kind) && "running".equals(state))
                || ("network".equals(kind) && "reconnecting".equals(state));
    }

    /**
     * Show `text` on `status`, hidden again after {@link #LINE_MS} unless it
     * `stays`. A later line cancels the hiding of an earlier one.
     */
    static void showLine(final TextView status, String text, boolean stays) {
        status.setText(text);
        status.setVisibility(text.isEmpty() ? View.GONE : View.VISIBLE);
        final Object shown = new Object();
        status.setTag(shown);
        if (stays || text.isEmpty()) {
            return;
        }
        status.postDelayed(new Runnable() {
            @Override
            public void run() {
                if (status.getTag() == shown) {
                    status.setVisibility(View.GONE);
                }
            }
        }, LINE_MS);
    }

    /** The status line for an event other than `changed`, or null. */
    static String eventLine(Context context, Map<String, Object> event) {
        if (event.containsKey("error")) {
            return Core.str(event, "error");
        }
        String kind = Core.str(event, "t");
        if ("notice".equals(kind)) {
            return Core.str(event, "text");
        }
        if ("sync".equals(kind)) {
            String state = Core.str(event, "state");
            if ("running".equals(state)) {
                return context.getString(R.string.pulling);
            }
            if ("done".equals(state)) {
                // The count comes as its digits (Event::Sync's detail).
                int count;
                try {
                    count = Integer.parseInt(Core.str(event, "detail"));
                } catch (NumberFormatException none) {
                    count = 0;
                }
                return count == 0
                        ? context.getString(R.string.pulled_nothing)
                        : context.getResources().getQuantityString(R.plurals.pulled, count, count);
            }
            // The library's words are for the log; the line says it tries again
            // (a down link in front pulls every minute, a wake pulls at once).
            Log.i(PhotoView.TAG, "pull failed: " + Core.str(event, "detail"));
            return context.getString(R.string.pull_failed);
        }
        if ("network".equals(kind)) {
            // "Reconnecting…" stays until the connection is back, then the
            // line is gone (1298).
            return "reconnecting".equals(Core.str(event, "state"))
                    ? context.getString(R.string.reconnecting) : "";
        }
        if ("stopped".equals(kind)) {
            String detail = Core.str(event, "detail");
            return detail.isEmpty() ? context.getString(R.string.stopped) : detail;
        }
        return null;
    }
}
