package network.leviculum.loquax;

import android.content.Context;
import android.view.View;
import android.widget.TextView;
import java.text.DateFormat;
import java.text.SimpleDateFormat;
import java.util.Calendar;
import java.util.Date;
import java.util.Locale;
import java.util.Map;

/** What the screens share: times and the status line's words. */
final class Ui {
    /** The library's answer to a call without a session (src/lib.rs). */
    static final String NOT_STARTED = "not started";

    private Ui() {}

    /** Whole days from `then` to `now` by the calendar, 0 for the same day. */
    static int daysBetween(long thenSecs, long nowSecs) {
        Calendar a = Calendar.getInstance();
        a.setTimeInMillis(thenSecs * 1000);
        Calendar b = Calendar.getInstance();
        b.setTimeInMillis(nowSecs * 1000);
        int days = 0;
        while (a.get(Calendar.YEAR) < b.get(Calendar.YEAR)
                || a.get(Calendar.DAY_OF_YEAR) < b.get(Calendar.DAY_OF_YEAR)
                && a.get(Calendar.YEAR) == b.get(Calendar.YEAR)) {
            a.add(Calendar.DAY_OF_YEAR, 1);
            days++;
            if (days > 7) {
                return days;
            }
        }
        return days;
    }

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
     * The conversation list's time (1130): now under a minute, minutes
     * under an hour, hours under a day, then the weekday within the week,
     * then the date, the year only when it is not this one. A time ahead of
     * the phone's clock is now.
     */
    static String listTime(Context context, long unixSecs, long nowSecs) {
        if (unixSecs <= 0) {
            return "";
        }
        long secs = nowSecs - unixSecs;
        if (secs < 60) {
            return context.getString(R.string.just_now);
        }
        if (secs < 3600) {
            return context.getString(R.string.minutes_ago, (int) (secs / 60));
        }
        if (secs < 86_400) {
            return context.getString(R.string.list_hours, (int) (secs / 3600));
        }
        Date at = new Date(unixSecs * 1000);
        if (daysBetween(unixSecs, nowSecs) < 7) {
            return new SimpleDateFormat("EEE", Locale.getDefault()).format(at);
        }
        Calendar then = Calendar.getInstance();
        then.setTime(at);
        Calendar now = Calendar.getInstance();
        now.setTimeInMillis(nowSecs * 1000);
        if (then.get(Calendar.YEAR) == now.get(Calendar.YEAR)) {
            String pattern = android.text.format.DateFormat.getBestDateTimePattern(Locale.getDefault(), "dMMM");
            return new SimpleDateFormat(pattern, Locale.getDefault()).format(at);
        }
        return DateFormat.getDateInstance(DateFormat.SHORT).format(at);
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
            case "delivered": return context.getString(R.string.state_delivered);
            case "read": return context.getString(R.string.state_read, clock(readAt));
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
        return "sync".equals(Core.str(event, "t")) && "running".equals(Core.str(event, "state"));
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
                String detail = Core.str(event, "detail");
                return "0".equals(detail)
                        ? context.getString(R.string.pulled_nothing)
                        : context.getString(R.string.pulled, detail);
            }
            return context.getString(R.string.pull_failed, Core.str(event, "detail"));
        }
        if ("stopped".equals(kind)) {
            String detail = Core.str(event, "detail");
            return detail.isEmpty() ? context.getString(R.string.stopped) : detail;
        }
        return null;
    }
}
