package network.leviculum.loquax;

import android.app.Activity;
import android.app.Dialog;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.location.Location;
import android.net.Uri;
import android.text.Editable;
import android.text.TextWatcher;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * The map as a main view and as a sub view (1199): the library's frame
 * ({@link MapView}) over the whole room between the bars, the credit at
 * the bottom, and a chip at the top edge only while something happens: a
 * region loading by itself where the user looks (the autoload of 1089),
 * no internet, no map where the user looks, a load that could not start.
 *
 * The map's people (1200): the friends are avatar pins with their age,
 * clustered where they overlap (the library's rule, friends.rs). A tap on
 * a pin opens its card at the bottom: name, age and distance (only with
 * the phone's own fix, which 1201 brings), Chat and Navigate, which hands
 * a geo: link to the phone's navigation app and is hidden when none takes
 * it. A tap on a cluster flies in until its pins part, or, where no zoom
 * parts them, opens the Friends sheet filtered to them. The round Friends
 * button bottom right opens the sheet: everyone with a position, the last
 * day first, older ones greyed under a divider; a tap flies the map to
 * the friend and opens the card. The map's search is that sheet with its
 * search field (friends only; places come later).
 *
 * Location (1201): the crosshair above the Friends button centres on the
 * phone's own position at about z16 on the first tap and follows it on
 * the second, until a finger moves the map. Its first tap ever asks for
 * the permission, with a line that says why (Locator); refused, the
 * crosshair keeps the view where the user looked and a chip says
 * "Location is off". While the map is in front and the permission is
 * there, the own position is a dot and the friends' distances are
 * measured from it. Nothing of it is sent: "Share my position" on the
 * empty map (to a chat the user picks) and "Share mine" on a friend's card
 * send it, once, to one peer.
 */
final class MapPane implements MapView.Listener {
    /** How long a chip that tells of an event stays. */
    private static final long CHIP_MS = 4000;
    /** The crosshair's zoom, about z16 (1201), in thousandths. */
    private static final long LOCATE_ZOOM = 16_000;

    /** What the crosshair did last: nothing, centred once, following. */
    private enum Locate { NONE, CENTRED, FOLLOWING }

    /** Where the pane goes beyond the map: a friend's chat. */
    interface Host {
        void chat(String peer);
    }

    private final Activity activity;
    private final Host host;
    private final TextView chip;
    private final TextView credit;
    private final ImageButton friendsButton;
    private final ImageButton locateButton;
    private final View empty;
    private final View card;
    private final Locator locator;
    private Locate locate = Locate.NONE;
    /** The crosshair was tapped and waits for the first fix to centre on. */
    private boolean locateOwed;
    /** The phone's own fix, while the map is in front and allowed to read it. */
    private Location fix;
    private final MapView map;
    /**
     * The tracks' time windows (1385), a row of chips under the event's
     * chip: the words the library takes, and the chips in that order.
     */
    private static final String[] TRACK_WORDS = {"today", "yesterday", "week", "month", "all"};
    private final TextView[] trackChips = new TextView[TRACK_WORDS.length];
    private final Runnable hide = new Runnable() {
        @Override
        public void run() {
            eventShown = false;
            chip.setVisibility(View.GONE);
            state(lastShown);
        }
    };
    /** An event's chip is up: the state chips wait until it goes. */
    private boolean eventShown;
    private Map<String, Object> lastShown;
    /** Everyone with a position, newest first, and the people by peer, ours among them. */
    private final List<Map<String, Object>> friends = new ArrayList<>();
    private final Map<String, Map<String, Object>> people = new HashMap<>();
    /** Our own address when the store has a position of ours: its card has no Chat. */
    private String own;
    /** The peer whose card is open, or whose card opens once the map has it. */
    private String carded;
    private String cardOwed;
    private Dialog sheet;

    /** The pane in `activity`'s layout (activity_conversations.xml). */
    MapPane(Activity activity, Host host) {
        this.activity = activity;
        this.host = host;
        chip = (TextView) activity.findViewById(R.id.chip);
        credit = (TextView) activity.findViewById(R.id.credit);
        card = activity.findViewById(R.id.card);
        friendsButton = (ImageButton) activity.findViewById(R.id.friends);
        friendsButton.setImageDrawable(new Glyph(Glyph.Kind.PEOPLE, activity.getColor(R.color.on_send)));
        friendsButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showSheet(null, false);
            }
        });
        locator = new Locator(activity);
        locateButton = (ImageButton) activity.findViewById(R.id.locate);
        tintLocate();
        locateButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                locate();
            }
        });
        empty = activity.findViewById(R.id.map_empty);
        activity.findViewById(R.id.map_share).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pickAndShare();
            }
        });
        map = new MapView(activity, activity.getFilesDir().getAbsolutePath(), this);
        ((FrameLayout) activity.findViewById(R.id.map)).addView(map);
        trackRow((FrameLayout) activity.findViewById(R.id.pane_map));
        refresh();
    }

    /**
     * The row of the tracks' time windows (1385) on `pane`, below the
     * event's chip: Java draws the chips and says which was pressed, the
     * library keeps the window and draws the tracks.
     */
    private void trackRow(FrameLayout pane) {
        int[] names = {
            R.string.scene_tracks_today, R.string.scene_tracks_yesterday,
            R.string.scene_tracks_week, R.string.scene_tracks_month, R.string.scene_tracks_all,
        };
        float dp = activity.getResources().getDisplayMetrics().density;
        LinearLayout row = new LinearLayout(activity);
        row.setOrientation(LinearLayout.HORIZONTAL);
        for (int i = 0; i < TRACK_WORDS.length; i++) {
            final String word = TRACK_WORDS[i];
            TextView chipView = new TextView(activity);
            chipView.setText(activity.getString(names[i]));
            chipView.setTextSize(13);
            chipView.setTextColor(activity.getColor(R.color.text));
            chipView.setBackgroundResource(R.drawable.chip);
            chipView.setElevation(2 * dp);
            int h = Math.round(12 * dp);
            int v = Math.round(6 * dp);
            chipView.setPadding(h, v, h, v);
            chipView.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View view) {
                    map.trackWindow(word);
                }
            });
            LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            at.setMarginEnd(Math.round(6 * dp));
            row.addView(chipView, at);
            trackChips[i] = chipView;
        }
        // "Fit all" (1386): every peer inside the time window in view.
        TextView fitChip = new TextView(activity);
        fitChip.setText(activity.getString(R.string.scene_fit_window));
        fitChip.setTextSize(13);
        fitChip.setTextColor(activity.getColor(R.color.text));
        fitChip.setBackgroundResource(R.drawable.chip);
        fitChip.setElevation(2 * dp);
        int fh = Math.round(12 * dp);
        int fv = Math.round(6 * dp);
        fitChip.setPadding(fh, fv, fh, fv);
        fitChip.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                map.fitWindow();
            }
        });
        row.addView(fitChip, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        HorizontalScrollView scroll = new HorizontalScrollView(activity);
        scroll.setHorizontalScrollBarEnabled(false);
        int side = Math.round(12 * dp);
        scroll.setPadding(side, 0, side, 0);
        scroll.setClipToPadding(false);
        scroll.addView(row);
        FrameLayout.LayoutParams place = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT,
                Gravity.TOP | Gravity.START);
        place.topMargin = Math.round(60 * dp);
        pane.addView(scroll, place);
    }

    /** The chip of the window the frame drew marked, the others not. */
    private void markTrackWindow(String drawn) {
        for (int i = 0; i < TRACK_WORDS.length; i++) {
            if (trackChips[i] != null) {
                boolean on = TRACK_WORDS[i].equals(drawn);
                trackChips[i].setSelected(on);
                trackChips[i].setAlpha(on ? 1f : 0.7f);
            }
        }
    }

    /**
     * Open on `peer`, as a row's pin does (1224): centred on its newest fix
     * at a zoom that shows the pin and its streets, its card opened once
     * the map has drawn there. A friend without a fix says so.
     */
    void openAt(String peer) {
        refresh();
        Map<String, Object> row = people.get(peer);
        if (row == null || row.get("lat_e6") == null || row.get("lon_e6") == null) {
            say(activity.getString(R.string.no_position));
            return;
        }
        if (carded != null && !carded.equals(peer)) {
            closeCard();
        }
        map.flyTo(Core.num(row, "lat_e6"), Core.num(row, "lon_e6"));
        cardOwed = peer;
    }

    /** Open on a place, as a message's fix does, with `peer`'s card open. */
    void centreOn(long latE6, long lonE6, String peer) {
        map.centreOn(latE6, lonE6, peer);
        cardOwed = peer;
    }

    /** Open on a place at `zoomMilli`, no card: the device walk's view (1193). */
    void centreAt(long latE6, long lonE6, long zoomMilli) {
        map.centreAt(latE6, lonE6, zoomMilli);
    }

    /**
     * Draw anew: the screen came to the front, or the pins changed. With
     * the permission there, the own position is heard of while in front.
     */
    void ask() {
        if (locator.allowed()) {
            watch();
        }
        refresh();
        map.ask();
    }

    /** The map left the front: the position is no longer read. */
    void pause() {
        locator.stop();
    }

    /** The system's answer to the permission question: whether it was the pane's. */
    boolean permission(int request) {
        return locator.result(request);
    }

    /** Hear of the own position: the dot, the distances, follow mode. */
    private void watch() {
        locator.watch(new Locator.Fix() {
            @Override
            public void on(Location location) {
                moved(location);
            }
        });
    }

    /** A new own fix: the dot moves, the distances change, follow mode follows. */
    private void moved(Location location) {
        boolean first = fix == null;
        fix = location;
        map.own(Math.round(location.getLatitude() * 1e6), Math.round(location.getLongitude() * 1e6));
        if (locateOwed && Locator.fresh(location)) {
            locateOwed = false;
            centre(LOCATE_ZOOM);
            locate = Locate.CENTRED;
            tintLocate();
        } else if (locate == Locate.FOLLOWING) {
            centre(map.zoom());
        }
        if (first) {
            refresh();
        }
    }

    /** Centre on the own fix at `zoomMilli`. */
    private void centre(long zoomMilli) {
        if (fix == null) {
            return;
        }
        map.centreAt(Math.round(fix.getLatitude() * 1e6), Math.round(fix.getLongitude() * 1e6),
                zoomMilli == 0 ? LOCATE_ZOOM : zoomMilli);
    }

    /**
     * The crosshair (1201): first tap centres at about z16, the second
     * follows; the first tap ever asks for the permission. Refused, the
     * view stays where the user looked and a chip says so.
     */
    private void locate() {
        locator.ask(new Runnable() {
            @Override
            public void run() {
                if (locate == Locate.CENTRED && fix != null) {
                    locate = Locate.FOLLOWING;
                    say(activity.getString(R.string.following));
                    centre(map.zoom());
                } else if (fix != null && Locator.fresh(fix)) {
                    locate = Locate.CENTRED;
                    centre(LOCATE_ZOOM);
                } else {
                    // No fix yet: the first that comes is centred on.
                    locateOwed = true;
                    say(activity.getString(R.string.location_finding));
                    watch();
                }
                tintLocate();
            }
        }, new Runnable() {
            @Override
            public void run() {
                locate = Locate.NONE;
                tintLocate();
                say(activity.getString(R.string.location_off));
            }
        });
    }

    /**
     * The crosshair's words for a screen reader: what the next tap does.
     * The look stays the one round button style; follow mode says itself
     * in a chip when it starts.
     */
    private void tintLocate() {
        boolean following = locate == Locate.FOLLOWING;
        locateButton.setImageDrawable(new Glyph(Glyph.Kind.LOCATE, activity.getColor(R.color.on_send)));
        locateButton.setSelected(following);
        locateButton.setContentDescription(activity.getString(
                following ? R.string.following : R.string.locate));
    }

    @Override
    public void onFinger() {
        if (locate != Locate.NONE || locateOwed) {
            locate = Locate.NONE;
            locateOwed = false;
            tintLocate();
        }
    }

    /**
     * "Share my position" on the empty map: the chats to pick from, then
     * the share to the one picked.
     */
    private void pickAndShare() {
        Map<String, Object> answer = Core.conversations(null);
        final List<Map<String, Object>> rows = Core.list(answer, "conversations");
        if (rows.isEmpty()) {
            say(activity.getString(R.string.share_nobody));
            return;
        }
        String[] names = new String[rows.size()];
        for (int i = 0; i < rows.size(); i++) {
            names[i] = Core.str(rows.get(i), "name");
        }
        new android.app.AlertDialog.Builder(activity)
                .setTitle(R.string.share_with)
                .setItems(names, new android.content.DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(android.content.DialogInterface dialog, int which) {
                        Map<String, Object> row = rows.get(which);
                        share(Core.str(row, "peer"), Core.str(row, "name"));
                    }
                })
                .show();
    }

    /** Share the own position with `peer`, once (1201). */
    private void share(String peer, String name) {
        locator.shareWith(peer, name, new Locator.Fix() {
            @Override
            public void on(Location location) {
                moved(location);
            }
        }, new Locator.Say() {
            @Override
            public void line(String text) {
                say(text);
            }
        });
    }

    /** The map's search (1200): the Friends sheet with its search field. */
    void search() {
        showSheet(null, true);
    }

    /** Back closes the card first: whether it did. */
    boolean back() {
        if (carded != null) {
            closeCard();
            return true;
        }
        return false;
    }

    /** The friends again from the library: the pins' faces, the card's words. */
    private void refresh() {
        Map<String, Object> answer = fix == null ? Core.friends()
                : Core.friends(Math.round(fix.getLatitude() * 1e6), Math.round(fix.getLongitude() * 1e6));
        if (answer.containsKey("error")) {
            return;
        }
        friends.clear();
        friends.addAll(Core.list(answer, "friends"));
        people.clear();
        own = null;
        for (Map<String, Object> row : friends) {
            people.put(Core.str(row, "peer"), row);
        }
        Object me = answer.get("me");
        if (me instanceof Map) {
            @SuppressWarnings("unchecked")
            Map<String, Object> ours = (Map<String, Object>) me;
            own = Core.str(ours, "peer");
            people.put(own, ours);
        }
        map.people(people, carded);
        // The pins drawn, and how many are nodes nearby (1302): the device
        // walk counts them here, the canvas has no nodes to dump.
        int nearby = 0;
        for (Map<String, Object> row : friends) {
            nearby += Core.bool(row, "nearby") ? 1 : 0;
        }
        Log.i(PhotoView.TAG, "map pins=" + friends.size() + " nearby=" + nearby);
        if (carded != null && people.containsKey(carded)) {
            fillCard(people.get(carded));
        }
        // The empty map (1201): one line and "Share my position".
        empty.setVisibility(friends.isEmpty() && carded == null ? View.VISIBLE : View.GONE);
    }

    @Override
    public void onShown(Map<String, Object> shown) {
        if (!Core.bool(shown, "ok")) {
            say(Core.str(shown, "error"));
            return;
        }
        lastShown = shown;
        credit.setText(Core.str(shown, "attribution"));
        markTrackWindow(Core.str(shown, "track_window"));
        if (shown.get("notice") != null) {
            say(Core.str(shown, "notice"));
        } else if (!eventShown) {
            state(shown);
        }
        if (cardOwed != null && people.containsKey(cardOwed)) {
            String peer = cardOwed;
            cardOwed = null;
            openCard(peer);
        }
    }

    /**
     * The chip of the map's state, when no event's chip is up: loading
     * for the area on screen (1292; a load for an area the view left says
     * nothing), offline, no map here; none when all is well.
     */
    private void state(Map<String, Object> shown) {
        if (shown == null) {
            return;
        }
        int words;
        if (Core.bool(shown, "loading_here")) {
            words = R.string.map_loading;
        } else if (Core.bool(shown, "offline")) {
            words = R.string.map_offline;
        } else if (Core.bool(shown, "no_map")) {
            words = R.string.map_no_map;
        } else {
            chip.setVisibility(View.GONE);
            return;
        }
        chip.removeCallbacks(hide);
        chip.setText(words);
        chip.setVisibility(View.VISIBLE);
    }

    @Override
    public void onMark(Map<String, Object> mark) {
        List<String> peers = new ArrayList<>();
        Object value = mark.get("peers");
        if (value instanceof List) {
            for (Object peer : (List<?>) value) {
                peers.add(String.valueOf(peer));
            }
        }
        if (peers.size() == 1) {
            openCard(peers.get(0));
            return;
        }
        closeCard();
        if (mark.get("split_milli") != null) {
            map.flyInto(mark);
        } else {
            showSheet(peers, false);
        }
    }

    @Override
    public void onTapAway() {
        closeCard();
    }

    /** `peer`'s card at the bottom, its pin in the accent ring. */
    private void openCard(String peer) {
        Map<String, Object> row = people.get(peer);
        if (row == null) {
            return;
        }
        carded = peer;
        fillCard(row);
        card.setVisibility(View.VISIBLE);
        friendsButton.setVisibility(View.GONE);
        locateButton.setVisibility(View.GONE);
        empty.setVisibility(View.GONE);
        map.people(people, carded);
    }

    private void closeCard() {
        carded = null;
        card.setVisibility(View.GONE);
        friendsButton.setVisibility(View.VISIBLE);
        locateButton.setVisibility(View.VISIBLE);
        empty.setVisibility(friends.isEmpty() ? View.VISIBLE : View.GONE);
        map.people(people, null);
    }

    /** The card's words and buttons for `row`. */
    private void fillCard(final Map<String, Object> row) {
        final String peer = Core.str(row, "peer");
        ((AvatarView) activity.findViewById(R.id.card_avatar)).show(peer, row);
        ((TextView) activity.findViewById(R.id.card_name)).setText(Core.str(row, "name"));
        // A node nearby that is no contact (1302): the card says so under
        // its announced name, and Add stands in place of Chat and Share.
        final boolean nearby = Core.bool(row, "nearby");
        ((TextView) activity.findViewById(R.id.card_line)).setText(nearby
                ? activity.getString(R.string.nearby_card) + " · " + cardLine(row)
                : cardLine(row));
        View add = activity.findViewById(R.id.card_add);
        add.setVisibility(nearby ? View.VISIBLE : View.GONE);
        add.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String why = Core.addContact(peer, Core.str(row, "announced"));
                if (why != null && !why.isEmpty()) {
                    say(why);
                    return;
                }
                host.chat(peer);
            }
        });
        View chat = activity.findViewById(R.id.card_chat);
        chat.setVisibility(peer.equals(own) || nearby ? View.GONE : View.VISIBLE);
        chat.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                host.chat(peer);
            }
        });
        View share = activity.findViewById(R.id.card_share);
        share.setVisibility(peer.equals(own) || nearby ? View.GONE : View.VISIBLE);
        share.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                share(peer, Core.str(row, "name"));
            }
        });
        final Intent navigate = new Intent(Intent.ACTION_VIEW, Uri.parse(Core.str(row, "geo")));
        View go = activity.findViewById(R.id.card_navigate);
        boolean handled = !Core.str(row, "geo").isEmpty()
                && navigate.resolveActivity(activity.getPackageManager()) != null;
        go.setVisibility(handled ? View.VISIBLE : View.GONE);
        go.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                try {
                    activity.startActivity(navigate);
                } catch (ActivityNotFoundException e) {
                    v.setVisibility(View.GONE);
                }
            }
        });
    }

    /** "5 min ago · 1.2 km": the age, and the distance only when the library gave one. */
    private String cardLine(Map<String, Object> row) {
        long at = Core.num(row, "at");
        String live = Core.str(row, "live");
        // A peer that reports says whether it is alive, in the chat row's
        // words as the library says them (1209, 1217); one known from a
        // text alone how old its fix is.
        String age = !live.isEmpty() ? live
                : Ui.nowSecs() - at < 60 ? activity.getString(R.string.just_now)
                : activity.getString(R.string.card_ago, Ui.age(activity, at, Ui.nowSecs()));
        String distance = distance(row);
        return distance == null ? age : age + " · " + distance;
    }

    /** The distance in the phone's way of writing numbers; null without one. */
    static String distance(Map<String, Object> row) {
        Object metres = row.get("distance_m");
        if (!(metres instanceof Number)) {
            return null;
        }
        long m = ((Number) metres).longValue();
        if (m < 1000) {
            return String.format(Locale.getDefault(), "%d m", m);
        }
        if (m < 10_000) {
            return String.format(Locale.getDefault(), "%.1f km", m / 1000.0);
        }
        return String.format(Locale.getDefault(), "%d km", Math.round(m / 1000.0));
    }

    /**
     * The Friends sheet (1200): everyone with a position, the last day
     * first, older ones greyed under a divider; only `only` when given (a
     * cluster no zoom parts); with the search field when `searching`. A
     * tap closes it and flies the map to the friend with the card open.
     */
    private void showSheet(final List<String> only, boolean searching) {
        refresh();
        if (sheet != null) {
            sheet.dismiss();
        }
        float density = activity.getResources().getDisplayMetrics().density;
        final int pad = Math.round(16 * density);
        final Dialog dialog = new Dialog(activity);
        dialog.requestWindowFeature(Window.FEATURE_NO_TITLE);
        LinearLayout root = new LinearLayout(activity);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setId(R.id.friends_sheet);
        root.setBackgroundResource(R.drawable.sheet);
        root.setPadding(0, Math.round(8 * density), 0, pad);
        View handle = new View(activity);
        handle.setBackgroundResource(R.drawable.handle);
        LinearLayout.LayoutParams handleAt = new LinearLayout.LayoutParams(Math.round(32 * density), Math.round(4 * density));
        handleAt.gravity = Gravity.CENTER_HORIZONTAL;
        handleAt.bottomMargin = Math.round(8 * density);
        root.addView(handle, handleAt);
        TextView title = new TextView(activity);
        title.setText(R.string.friends);
        title.setTextColor(activity.getColor(R.color.text));
        title.setTextSize(20);
        title.setPadding(pad, 0, pad, Math.round(8 * density));
        root.addView(title);
        final EditText query = new EditText(activity);
        query.setHint(R.string.friends_search);
        query.setSingleLine(true);
        query.setTextColor(activity.getColor(R.color.text));
        query.setHintTextColor(activity.getColor(R.color.text_secondary));
        query.setBackground(null);
        query.setPadding(pad, Math.round(4 * density), pad, Math.round(8 * density));
        query.setVisibility(searching ? View.VISIBLE : View.GONE);
        root.addView(query);
        final LinearLayout rows = new LinearLayout(activity);
        rows.setOrientation(LinearLayout.VERTICAL);
        ScrollView scroll = new ScrollView(activity);
        scroll.addView(rows);
        root.addView(scroll, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT));
        final Runnable fill = new Runnable() {
            @Override
            public void run() {
                fillSheet(rows, only, query.getText().toString(), dialog);
            }
        };
        query.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                fill.run();
            }
        });
        fill.run();
        dialog.setContentView(root);
        Window window = dialog.getWindow();
        if (window != null) {
            window.setBackgroundDrawable(new ColorDrawable(Color.TRANSPARENT));
            window.setGravity(Gravity.BOTTOM);
            window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            if (searching) {
                window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE);
            }
        }
        if (searching) {
            query.requestFocus();
        }
        sheet = dialog;
        dialog.show();
    }

    /** The sheet's rows into `rows`: `only` when given, those whose name holds `needle`. */
    private void fillSheet(LinearLayout rows, List<String> only, String needle, final Dialog dialog) {
        rows.removeAllViews();
        float density = activity.getResources().getDisplayMetrics().density;
        int pad = Math.round(16 * density);
        String find = needle.trim().toLowerCase(Locale.getDefault());
        boolean divided = false;
        int shown = 0;
        for (final Map<String, Object> row : friends) {
            String peer = Core.str(row, "peer");
            if (only != null && !only.contains(peer)) {
                continue;
            }
            if (!find.isEmpty() && !Core.str(row, "name").toLowerCase(Locale.getDefault()).contains(find)) {
                continue;
            }
            boolean recent = Core.bool(row, "recent");
            if (!recent && !divided) {
                divided = true;
                TextView divider = new TextView(activity);
                divider.setId(R.id.friends_older);
                divider.setText(R.string.friends_older);
                divider.setTextColor(activity.getColor(R.color.text_secondary));
                divider.setTextSize(13);
                divider.setPadding(pad, Math.round(12 * density), pad, Math.round(4 * density));
                rows.addView(divider);
            }
            rows.addView(sheetRow(row, recent, dialog));
            shown++;
        }
        if (shown == 0) {
            TextView none = new TextView(activity);
            none.setId(R.id.friends_empty);
            none.setText(friends.isEmpty() ? R.string.friends_none : R.string.friends_no_match);
            none.setTextColor(activity.getColor(R.color.text_secondary));
            none.setTextSize(15);
            none.setPadding(pad, pad, pad, pad);
            rows.addView(none);
        }
    }

    /** One friend in the sheet: avatar, name, age, distance; greyed when older than a day. */
    private View sheetRow(final Map<String, Object> row, boolean recent, final Dialog dialog) {
        float density = activity.getResources().getDisplayMetrics().density;
        int pad = Math.round(16 * density);
        LinearLayout line = new LinearLayout(activity);
        line.setId(R.id.friend_row);
        line.setOrientation(LinearLayout.HORIZONTAL);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(pad, Math.round(8 * density), pad, Math.round(8 * density));
        line.setBackgroundResource(android.R.drawable.list_selector_background);
        line.setAlpha(recent ? 1f : 0.55f);
        final String peer = Core.str(row, "peer");
        AvatarView avatar = new AvatarView(activity, null);
        avatar.show(peer, row);
        int side = Math.round(40 * density);
        line.addView(avatar, new LinearLayout.LayoutParams(side, side));
        TextView name = new TextView(activity);
        name.setId(R.id.friend_name);
        name.setText(Core.str(row, "name"));
        name.setTextColor(activity.getColor(R.color.text));
        name.setTextSize(16);
        name.setSingleLine(true);
        name.setEllipsize(android.text.TextUtils.TruncateAt.END);
        LinearLayout.LayoutParams nameAt = new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        nameAt.setMarginStart(pad);
        line.addView(name, nameAt);
        TextView age = new TextView(activity);
        age.setId(R.id.friend_age);
        age.setText(Ui.age(activity, Core.num(row, "at"), Ui.nowSecs()));
        age.setTextColor(activity.getColor(R.color.text_secondary));
        age.setTextSize(14);
        age.setPadding(pad, 0, 0, 0);
        line.addView(age);
        String far = distance(row);
        if (far != null) {
            TextView distance = new TextView(activity);
            distance.setText(far);
            distance.setTextColor(activity.getColor(R.color.text_secondary));
            distance.setTextSize(14);
            distance.setMinWidth(Math.round(64 * density));
            distance.setGravity(Gravity.END);
            line.addView(distance);
        }
        line.setContentDescription(Core.str(row, "name"));
        line.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                dialog.dismiss();
                fly(peer, row);
            }
        });
        return line;
    }

    /** Fly the map to `row`'s place and open its card. */
    private void fly(String peer, Map<String, Object> row) {
        map.flyTo(Core.num(row, "lat_e6"), Core.num(row, "lon_e6"));
        openCard(peer);
    }

    /** A line from the session the map shows as a chip; null leaves the chip. */
    void event(String line) {
        if (line != null && !line.isEmpty()) {
            say(line);
        }
    }

    /** A chip for an event: it goes after a while, and the map's state shows again. */
    private void say(String text) {
        eventShown = true;
        chip.setText(text);
        chip.setVisibility(View.VISIBLE);
        chip.removeCallbacks(hide);
        chip.postDelayed(hide, CHIP_MS);
    }
}
