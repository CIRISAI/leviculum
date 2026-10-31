package network.leviculum.loquax;

import android.app.Activity;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.TextView;
import java.util.Map;

/**
 * The map as a main view and as a sub view (1199): the library's frame
 * ({@link MapView}) over the whole room between the bars, the credit at
 * the bottom, and a chip at the top edge only while something happens: a
 * region loading by itself where the user looks (the autoload of 1089),
 * a load that could not start, a pin tapped. No buttons: the theme is
 * Settings, Appearance; regions load by themselves.
 */
final class MapPane implements MapView.Listener {
    /** How long a chip that tells of an event stays. */
    private static final long CHIP_MS = 4000;

    private final TextView chip;
    private final TextView credit;
    private final MapView map;
    private final Runnable hide = new Runnable() {
        @Override
        public void run() {
            chip.setVisibility(View.GONE);
        }
    };
    /** The chip shows the load: it goes by itself when the load ends. */
    private boolean loadingShown;

    /** The pane in `activity`'s layout (activity_conversations.xml). */
    MapPane(Activity activity) {
        chip = (TextView) activity.findViewById(R.id.chip);
        credit = (TextView) activity.findViewById(R.id.credit);
        map = new MapView(activity, activity.getFilesDir().getAbsolutePath(), this);
        ((FrameLayout) activity.findViewById(R.id.map)).addView(map);
    }

    /** Open on a place, as a message's fix does, with `peer`'s pin selected. */
    void centreOn(long latE6, long lonE6, String peer) {
        map.centreOn(latE6, lonE6, peer);
    }

    /** Draw anew: the screen came to the front, or the pins changed. */
    void ask() {
        map.ask();
    }

    @Override
    public void onShown(Map<String, Object> shown) {
        if (!Core.bool(shown, "ok")) {
            say(Core.str(shown, "error"));
            return;
        }
        credit.setText(Core.str(shown, "attribution"));
        if (shown.get("tapped_name") != null) {
            say(Core.str(shown, "tapped_name"));
        } else if (shown.get("notice") != null) {
            say(Core.str(shown, "notice"));
        } else if (Core.bool(shown, "loading")) {
            chip.removeCallbacks(hide);
            chip.setText(R.string.map_loading);
            chip.setVisibility(View.VISIBLE);
            loadingShown = true;
        } else if (loadingShown) {
            loadingShown = false;
            chip.setVisibility(View.GONE);
        }
    }

    /** A line from the session the map shows as a chip; null leaves the chip. */
    void event(String line) {
        if (line != null) {
            say(line);
        }
    }

    /** A chip for an event: it goes after a while. */
    private void say(String text) {
        loadingShown = false;
        chip.setText(text);
        chip.setVisibility(View.VISIBLE);
        chip.removeCallbacks(hide);
        chip.postDelayed(hide, CHIP_MS);
    }
}
