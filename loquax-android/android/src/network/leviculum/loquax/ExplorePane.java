package network.leviculum.loquax;

import android.app.Activity;
import android.graphics.Typeface;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.util.List;
import java.util.Map;

/**
 * Explore (1578): the pages and the public rooms under one button, as the
 * window and the terminal carry it. On top the search field, which also
 * takes a pasted address, and the order's toggle; the pages under their
 * headings (bookmarks, today, this week, older), a tap opens one in the
 * browser, a long press or the star bookmarks it; then the rooms the
 * reader is in, the hubs Loquax ships, and the hubs heard on the mesh.
 * Every word and every order is the library's ({@link Core#exploreScreen});
 * a tap is a verb ({@link Core#explore}) and what the library then shows
 * (the list, a page, a hub's rooms) the Activity follows.
 */
final class ExplorePane {
    /** What the pane hands back to the Activity. */
    interface Host {
        /** The tab gave way to the chats. */
        void left();

        /** A page it opened is in front: the browser. */
        void page();

        /** A hub's rooms it opened are in front. */
        void rooms();
    }

    private final Activity screen;
    private final Host host;
    private final EditText field;
    private final TextView sort;
    private final LinearLayout list;
    /** The field's text as the library last had it: no echo of a draw back as a verb. */
    private String typed = "";
    /** What the tab showed last: Back decides by it. */
    private Map<String, Object> last;

    ExplorePane(Activity activity, Host host) {
        this.screen = activity;
        this.host = host;
        FrameLayout pane = (FrameLayout) activity.findViewById(R.id.pane_explore);
        LinearLayout column = new LinearLayout(activity);
        column.setOrientation(LinearLayout.VERTICAL);
        pane.addView(column, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        LinearLayout top = new LinearLayout(activity);
        top.setGravity(Gravity.CENTER_VERTICAL);
        top.setPadding(dp(12), dp(8), dp(12), dp(4));
        field = new EditText(activity);
        field.setSingleLine(true);
        field.setImeOptions(EditorInfo.IME_ACTION_GO);
        field.setId(R.id.explore_search);
        field.setTextColor(activity.getColor(R.color.text));
        field.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {
            }

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {
            }

            @Override
            public void afterTextChanged(Editable s) {
                String now = s.toString();
                if (!now.equals(typed)) {
                    typed = now;
                    verb("typed", now, "", 0);
                }
            }
        });
        field.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, android.view.KeyEvent event) {
                verb("address", "", "", 0);
                return true;
            }
        });
        top.addView(field, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        sort = new TextView(activity);
        sort.setTextSize(13);
        sort.setPadding(dp(12), dp(8), dp(4), dp(8));
        sort.setTextColor(activity.getColor(R.color.accent));
        sort.setId(R.id.explore_sort);
        sort.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("az", "", "", Core.bool(last, "az") ? 0 : 1);
            }
        });
        top.addView(sort);
        column.addView(top);

        ScrollView scroll = new ScrollView(activity);
        list = new LinearLayout(activity);
        list.setOrientation(LinearLayout.VERTICAL);
        list.setId(R.id.explore_list);
        scroll.addView(list);
        column.addView(scroll, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));
    }

    /** The tab came to the front: its list, nothing it opened. */
    void show() {
        verb("tab", "", "", 0);
    }

    /** Back: the field cleared; false with an empty field, as last drawn. */
    boolean back() {
        if (last == null || Core.str(last, "query").isEmpty()) {
            return false;
        }
        field.setText("");
        return true;
    }

    /** A verb on the engine's thread (1446); the tab drawn after it. */
    private void verb(final String verb, final String text, final String dest, final int index) {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.explore(verb, text, dest, index);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                if (error != null) {
                    Toast.makeText(screen, error, Toast.LENGTH_SHORT).show();
                }
                draw();
            }
        });
    }

    /** Ask the library on the engine's thread (1464) and draw its answer. */
    void draw() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.exploreScreen();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                if (screen.isDestroyed()) {
                    return;
                }
                show(shown);
            }
        });
    }

    private void show(Map<String, Object> shown) {
        last = shown;
        String showing = Core.str(shown, "showing");
        if (showing.equals("browse")) {
            host.page();
            return;
        }
        if (showing.equals("rooms")) {
            host.rooms();
            return;
        }
        if (!showing.equals("list")) {
            host.left();
            return;
        }
        String query = Core.str(shown, "query");
        if (!query.equals(field.getText().toString())) {
            typed = query;
            field.setText(query);
            field.setSelection(query.length());
        }
        field.setHint(Core.str(shown, "placeholder"));
        sort.setText(Core.str(shown, "sort"));
        list.removeAllViews();
        list.addView(title(Core.str(shown, "pages_title")));
        note(Core.str(shown, "pages_off"));
        String address = Core.str(shown, "address");
        if (!address.isEmpty()) {
            TextView open = line(address, 17, R.color.accent);
            open.setId(R.id.explore_address);
            open.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    verb("address", "", "", 0);
                }
            });
            list.addView(open);
        }
        for (Map<String, Object> page : Core.list(shown, "pages")) {
            String heading = Core.str(page, "heading");
            if (!heading.isEmpty()) {
                list.addView(heading(heading));
            }
            list.addView(page(page));
        }
        note(Core.str(shown, "pages_empty"));
        list.addView(title(Core.str(shown, "rooms_title")));
        List<Map<String, Object>> joined = Core.list(shown, "joined");
        for (int i = 0; i < joined.size(); i++) {
            list.addView(room(joined.get(i), i));
        }
        for (Map<String, Object> hub : Core.list(shown, "hubs")) {
            list.addView(hub(hub));
        }
        List<Map<String, Object>> heard = Core.list(shown, "heard");
        if (!heard.isEmpty()) {
            list.addView(heading(Core.str(shown, "heard_title")));
            for (Map<String, Object> hub : heard) {
                list.addView(hub(hub));
            }
        }
        note(Core.str(shown, "rooms_empty"));
    }

    /** A page's row: its name over when it was seen; a tap opens, a long press or the star bookmarks. */
    private View page(Map<String, Object> page) {
        final String dest = Core.str(page, "dest");
        LinearLayout row = new LinearLayout(screen);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(8), dp(8), dp(8));
        row.setBackgroundResource(android.R.drawable.list_selector_background);
        row.setTag("explore_page_" + dest);
        LinearLayout words = new LinearLayout(screen);
        words.setOrientation(LinearLayout.VERTICAL);
        TextView name = new TextView(screen);
        name.setText(Core.str(page, "label"));
        name.setTextSize(17);
        name.setTextColor(screen.getColor(R.color.text));
        words.addView(name);
        String said = Core.str(page, "said");
        if (!said.isEmpty()) {
            TextView seen = new TextView(screen);
            seen.setText(said);
            seen.setTextSize(13);
            seen.setTextColor(screen.getColor(R.color.text_secondary));
            words.addView(seen);
        }
        row.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        TextView star = new TextView(screen);
        star.setText(Core.bool(page, "bookmarked") ? "★" : "☆");
        star.setTextSize(22);
        star.setPadding(dp(12), 0, dp(8), 0);
        star.setTextColor(screen.getColor(R.color.accent));
        star.setTag("explore_star_" + dest);
        star.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("bookmark", "", dest, 0);
            }
        });
        row.addView(star);
        row.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("open", "", dest, 0);
            }
        });
        row.setOnLongClickListener(new View.OnLongClickListener() {
            @Override
            public boolean onLongClick(View v) {
                verb("bookmark", "", dest, 0);
                return true;
            }
        });
        return row;
    }

    /** A room the reader is in: its name, the globe and its word, the unread. */
    private View room(Map<String, Object> room, final int index) {
        long unread = Core.num(room, "unread");
        String words = Core.str(room, "name") + "  🌐 " + Core.str(room, "public")
                + (unread > 0 ? "  (" + unread + ")" : "");
        TextView row = line(words, 17, R.color.text);
        if (unread > 0) {
            row.setTypeface(null, Typeface.BOLD);
        }
        row.setTag("explore_room_" + index);
        row.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("room", "", "", index);
            }
        });
        return row;
    }

    /** A hub: its name, the mark on the one in force, how far. */
    private View hub(Map<String, Object> hub) {
        final String dest = Core.str(hub, "dest");
        String value = Core.str(hub, "value");
        String words = Core.str(hub, "name") + (Core.bool(hub, "chosen") ? "  ✓" : "")
                + (value.isEmpty() ? "" : "  · " + value);
        TextView row = line(words, 17, R.color.text);
        row.setTag("explore_hub_" + dest);
        row.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("hub", "", dest, 0);
            }
        });
        return row;
    }

    private TextView line(String words, int size, int colour) {
        TextView view = new TextView(screen);
        view.setText(words);
        view.setTextSize(size);
        view.setTextColor(screen.getColor(colour));
        view.setPadding(dp(16), dp(10), dp(16), dp(10));
        view.setBackgroundResource(android.R.drawable.list_selector_background);
        return view;
    }

    private TextView title(String words) {
        TextView view = new TextView(screen);
        view.setText(words);
        view.setTextSize(18);
        view.setTypeface(null, Typeface.BOLD);
        view.setTextColor(screen.getColor(R.color.text));
        view.setPadding(dp(16), dp(16), dp(16), dp(4));
        return view;
    }

    private TextView heading(String words) {
        TextView view = new TextView(screen);
        view.setText(words);
        view.setTextSize(13);
        view.setTextColor(screen.getColor(R.color.text_secondary));
        view.setPadding(dp(16), dp(10), dp(16), dp(2));
        return view;
    }

    /** One sentence of the library's, where it says one. */
    private void note(String words) {
        if (!words.isEmpty()) {
            TextView view = heading(words);
            view.setPadding(dp(16), dp(8), dp(16), dp(8));
            list.addView(view);
        }
    }

    private int dp(int value) {
        return (int) (value * screen.getResources().getDisplayMetrics().density + 0.5f);
    }
}
