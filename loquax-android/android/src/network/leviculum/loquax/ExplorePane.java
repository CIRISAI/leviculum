package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.DialogInterface;
import android.graphics.Typeface;
import android.os.SystemClock;
import android.text.Editable;
import android.text.TextUtils;
import android.text.TextWatcher;
import android.view.Gravity;
import android.view.Menu;
import android.view.MenuItem;
import android.view.MotionEvent;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.PopupMenu;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * Explore (1578): the pages and the public rooms under one button, as the
 * window and the terminal carry it. On top the search field, which also
 * takes a pasted address, and the order's toggle; under them one list:
 * the pages under their headings (bookmarks, today, this week, older), a
 * tap opens one in the browser, the star bookmarks it, a long press opens
 * its menu; then the rooms the reader is in, the servers Loquax ships and
 * those heard on the mesh, a tap opening a server's sheet. Every word and
 * every order is the library's ({@link Core#exploreScreen}); a tap is a
 * verb ({@link Core#explore}) and what the library then shows (the list,
 * a page, a server's rooms) the Activity follows.
 *
 * <p>Since 1618 (the walk of 17024c3a on the Pixel) the list is a
 * {@link ListView}: only the visible rows are views, every row has a
 * stable id, and a new answer keeps the scroll; a search starts at the
 * top. The field is the source of truth and an answer to an older
 * {@code typed} verb never writes it back, and an answer that comes while
 * a finger is on the list waits for the finger to lift
 * ({@link ExploreRows}).
 *
 * <p>Since 1621 (walk 2, 02e6984a) the list stands still between looking
 * and tapping: an answer shows at once only when no row on screen moves,
 * the first row on screen put back at its pixel; one that would move a
 * row waits behind the chip "N neue Seiten" floating over the list (1629),
 * a tap on which shows it from the top ({@link ExploreRows}); at the top
 * and left alone, the list fills by itself (1629). The rooms' section is
 * "Räume" with the rooms the reader is in, then "Server" with every server.
 */
final class ExplorePane {
    /** What the pane hands back to the Activity. */
    interface Host {
        /** The tab gave way to the chats. */
        void left();

        /** A page it opened is in front: the browser, for the page named {@code name}. */
        void page(String name);

        /** A server's rooms it opened are in front. */
        void rooms();
    }

    /** One row of the list: what it is, its key and its words. */
    private static final class Row {
        static final int TITLE = 0;
        static final int NOTE = 1;
        static final int HEADING = 2;
        static final int ADDRESS = 3;
        static final int PAGE = 4;
        static final int ROOM = 5;
        static final int SERVER = 6;
        /** "Räume öffnen" (1655). */
        static final int OPEN = 7;
        static final int KINDS = 8;

        final int kind;
        final String key;
        final String words;
        final Map<String, Object> item;
        final int index;

        Row(int kind, String key, String words, Map<String, Object> item, int index) {
            this.kind = kind;
            this.key = key;
            this.words = words;
            this.item = item;
            this.index = index;
        }
    }

    private final Activity screen;
    private final Host host;
    private final EditText field;
    private final TextView sort;
    private final ListView list;
    /** The chip over the list: new pages wait behind it (1621). */
    private final TextView fresh;
    private final ExploreRows<Row> rows = new ExploreRows<Row>(new ExploreRows.Keys<Row>() {
        @Override
        public String key(Row row) {
            return row.key;
        }

        @Override
        public boolean counts(Row row) {
            return row.kind == Row.PAGE;
        }
    });
    /** The name of the page a tap opened, for the browser's first frame (1632). */
    private String opening = "";
    /** The next answer is the reader's own verb's: shown at once. */
    private boolean forced = true;
    private final Adapter adapter = new Adapter();
    /** The field's text as last sent: no echo of a draw back as a verb. */
    private String typed = "";
    /** `typed` verbs sent and not yet answered. */
    private int inFlight;
    /** The query the list was last drawn for: a new one starts at the top. */
    private String drawnFor;
    /** What the tab showed last: Back decides by it. */
    private Map<String, Object> last;
    /** The next answer comes after Back from what the list opened (1659, walk 10, 50). */
    private boolean returning;
    /** The bar over the list (1659, walk 10, 54): Seiten · Räume · Server, a tap jumps there. */
    private final LinearLayout jumps;
    private final TextView[] jump = new TextView[3];
    private static final String[] JUMP_TO = {"title:pages", "title:rooms", "heading:servers"};
    private static final String[] JUMP_WORDS = {"pages_title", "rooms_title", "servers_title"};
    private static final String[] JUMP_TAGS = {"explore_jump_pages", "explore_jump_rooms",
        "explore_jump_servers"};
    /** The server's sheet on screen, its server, its body and what it shows. */
    private AlertDialog sheet;
    private String sheetFor = "";
    private LinearLayout sheetBody;
    private String sheetShows = "";

    ExplorePane(Activity activity, Host host) {
        this.screen = activity;
        this.host = host;
        FrameLayout pane = (FrameLayout) activity.findViewById(R.id.pane_explore);
        // The column under the chip: the chip sits on the edge between the
        // search bar and the list (1632, walk 4 new 18: floating 8 dp into
        // the list it hid the line of the row under it).
        final FrameLayout head = new FrameLayout(activity);
        pane.addView(head, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        LinearLayout column = new LinearLayout(activity);
        column.setOrientation(LinearLayout.VERTICAL);
        head.addView(column, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        final LinearLayout top = new LinearLayout(activity);
        top.setGravity(Gravity.CENTER_VERTICAL);
        top.setPadding(dp(12), dp(4), dp(12), dp(2));
        field = new EditText(activity);
        field.setSingleLine(true);
        field.setImeOptions(EditorInfo.IME_ACTION_GO);
        field.setId(R.id.explore_search);
        field.setTextColor(activity.getColor(R.color.text));
        field.setHintTextColor(activity.getColor(R.color.text_secondary));
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
                    inFlight++;
                    forced = true;
                    verb("typed", now, "", 0, true);
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
        // As wide as the longer of its two words, whole (walk 2: "Zuletzt
        // gesehen zu…"), so the field's width never moves with them (walk
        // 13 of 1618: 685 or 911 px).
        sort = new TextView(activity);
        sort.setTextSize(13);
        sort.setSingleLine(true);
        sort.setGravity(Gravity.END | Gravity.CENTER_VERTICAL);
        sort.setPadding(dp(8), dp(8), dp(4), dp(8));
        sort.setTextColor(activity.getColor(R.color.accent));
        sort.setId(R.id.explore_sort);
        float words = Math.max(
                sort.getPaint().measureText(activity.getString(R.string.explore_sort_seen)),
                sort.getPaint().measureText(activity.getString(R.string.explore_sort_az)));
        sort.setMinWidth((int) Math.ceil(words) + sort.getPaddingLeft() + sort.getPaddingRight());
        sort.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                forced = true;
                verb("az", "", "", Core.bool(last, "az") ? 0 : 1);
            }
        });
        top.addView(sort, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        column.addView(top);

        // Three chips that jump to the list's sections (1659, walk 10, 54:
        // "Räume öffnen" was 20 swipes under 100 pages). The sections keep
        // their order; the bar stays over the list.
        jumps = new LinearLayout(activity);
        jumps.setPadding(dp(12), 0, dp(12), dp(4));
        jumps.setGravity(Gravity.CENTER_VERTICAL);
        for (int i = 0; i < jump.length; i++) {
            final int section = i;
            TextView chipView = new TextView(activity);
            chipView.setTextSize(14);
            chipView.setSingleLine(true);
            chipView.setTextColor(activity.getColor(R.color.accent));
            chipView.setPadding(dp(14), dp(6), dp(14), dp(6));
            android.graphics.drawable.GradientDrawable outline =
                    new android.graphics.drawable.GradientDrawable();
            outline.setCornerRadius(dp(16));
            outline.setStroke(dp(1), activity.getColor(R.color.accent));
            chipView.setBackground(outline);
            chipView.setTag(JUMP_TAGS[i]);
            chipView.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    jumpTo(section);
                }
            });
            LinearLayout.LayoutParams gap = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            gap.setMarginEnd(dp(8));
            jumps.addView(chipView, gap);
            jump[i] = chipView;
        }
        column.addView(jumps);

        // New pages that would move a row on screen wait behind this chip
        // (1621); a tap shows them, from the top. It floats over the list
        // (1629, walk 3: as a row above it, it pushed every row 87 px down
        // when it came), as the chat apps' "new posts" pill does.
        fresh = new TextView(activity);
        fresh.setTextSize(14);
        fresh.setTypeface(null, Typeface.BOLD);
        fresh.setTextColor(android.graphics.Color.WHITE);
        fresh.setGravity(Gravity.CENTER);
        fresh.setPadding(dp(16), dp(8), dp(16), dp(8));
        android.graphics.drawable.GradientDrawable pill =
                new android.graphics.drawable.GradientDrawable();
        pill.setColor(activity.getColor(R.color.accent));
        pill.setCornerRadius(dp(18));
        fresh.setBackground(pill);
        fresh.setElevation(dp(4));
        fresh.setTag("explore_fresh");
        fresh.setVisibility(View.GONE);
        fresh.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                boolean applied = rows.apply();
                // The band back to its resting 20 dp first (1653, walk 8,
                // 38), then the list to its top under it.
                chip();
                if (applied) {
                    adapter.notifyDataSetChanged();
                    // After the list laid the new rows out: a selection set
                    // before that is taken back by the stable ids, which
                    // keep the old first row (walk 3: the tap stayed put).
                    list.post(new Runnable() {
                        @Override
                        public void run() {
                            measureBand();
                            list.setSelectionFromTop(0, 0);
                        }
                    });
                }
            }
        });

        list = new ListView(activity) {
            @Override
            public boolean dispatchTouchEvent(MotionEvent event) {
                // The press before ListView takes it, the lift after
                // ListView decided the click (1653, walk 8, 39): whatever
                // the lift does to the rows then cannot cost the tap.
                int action = event.getActionMasked();
                boolean lift = action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL;
                if (!lift) {
                    finger(event);
                }
                boolean handled = super.dispatchTouchEvent(event);
                if (lift) {
                    finger(event);
                }
                return handled;
            }
        };
        list.setId(R.id.explore_list);
        list.setDivider(null);
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                // The row the finger came down on, not the one at the
                // position now (1632, walk 4: an answer between the lift
                // and this click had put the row above there).
                tapped(rows.hit(position));
                // What came while the click was owed, now (1659, walk 10, 51).
                settled();
            }
        });
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                Row row = rows.hit(position);
                settled();
                if (row.kind != Row.PAGE) {
                    return false;
                }
                menu(view, row.item);
                return true;
            }
        });
        list.setOnScrollListener(new android.widget.AbsListView.OnScrollListener() {
            @Override
            public void onScrollStateChanged(android.widget.AbsListView view, int state) {
                if (state != SCROLL_STATE_IDLE) {
                    rows.touched(SystemClock.uptimeMillis());
                    rows.release();
                } else {
                    // At rest again, maybe at the top: what waits shows
                    // once the list rested there long enough.
                    restLater();
                }
            }

            @Override
            public void onScroll(android.widget.AbsListView view, int first, int visible, int total) {
            }
        });
        // The chip's lower half has a band of its own at the list's top,
        // always there so nothing moves when the chip comes; rows scroll
        // through it.
        list.setPadding(0, dp(20), 0, 0);
        list.setClipToPadding(false);
        column.addView(list, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));
        FrameLayout.LayoutParams over = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
                Gravity.TOP | Gravity.CENTER_HORIZONTAL);
        head.addView(fresh, over);
        // Centred on the bar's bottom edge: half over the bar's padding and
        // field line, half over the list's first few pixels, not a row's
        // words.
        View.OnLayoutChangeListener edge = new View.OnLayoutChangeListener() {
            @Override
            public void onLayoutChange(View v, int l, int t, int r, int b, int ol, int ot, int or, int ob) {
                fresh.setTranslationY(jumps.getBottom() - fresh.getHeight() / 2f);
            }
        };
        top.addOnLayoutChangeListener(edge);
        jumps.addOnLayoutChangeListener(edge);
        fresh.addOnLayoutChangeListener(edge);
    }

    /**
     * The list stands at its very top: the first row at or under the
     * band's resting 20 dp (the chip's band, {@link #band}, is deeper and
     * would otherwise take a list kept at its top for a scrolled one).
     */
    private boolean atTop() {
        return list.getChildCount() == 0
                || (list.getFirstVisiblePosition() == 0 && topOfFirst() >= dp(20));
    }

    /** What waits behind the chip, offered again as the screen stands now (1629). */
    private final Runnable offerWaiting = new Runnable() {
        @Override
        public void run() {
            if (rows.waits() && atTop() && !screen.isDestroyed()) {
                draw();
            }
        }
    };

    /** Offer what waits once the list rested {@link ExploreRows#QUIET_MS}. */
    private void restLater() {
        list.removeCallbacks(offerWaiting);
        list.postDelayed(offerWaiting, ExploreRows.QUIET_MS + 50);
    }

    /** The tab came to the front: its list, nothing it opened. */
    void show() {
        forced = true;
        returning = false;
        verb("tab", "", "", 0);
    }

    /**
     * Back from a page or the rooms the list opened (1659, walk 10, 50):
     * the list as it stood, the chip with it; no verb of the reader's.
     */
    void returned() {
        forced = false;
        returning = true;
        verb("tab", "", "", 0);
    }

    /** A jump chip: the section's heading to the list's top (1659, walk 10, 54). */
    private void jumpTo(int section) {
        int at = rows.find(JUMP_TO[section]);
        if (at < 0) {
            return;
        }
        rows.touched(SystemClock.uptimeMillis());
        rows.release();
        list.setSelectionFromTop(at, 0);
        restLater();
    }

    /** The click owed ran, or will not come: what came meanwhile is offered (1659, 51). */
    private void settled() {
        list.removeCallbacks(settleLater);
        int top = topOfFirst();
        int said = rows.settle(firstOnScreen(), list.getLastVisiblePosition());
        if (said == ExploreRows.SHOWN) {
            place(top);
        } else if (said == ExploreRows.WAITING) {
            rebind();
        }
        chip();
    }

    /** No click came after the lift (a row that takes none): settle anyway. */
    private final Runnable settleLater = new Runnable() {
        @Override
        public void run() {
            if (!screen.isDestroyed()) {
                settled();
            }
        }
    };

    /** The bar's magnifier on Explore (walk 14): the tab's own field, with the keyboard. */
    void focus() {
        field.requestFocus();
        InputMethodManager keyboard =
                (InputMethodManager) screen.getSystemService(Context.INPUT_METHOD_SERVICE);
        keyboard.showSoftInput(field, InputMethodManager.SHOW_IMPLICIT);
    }

    /** Back: the field cleared; false with an empty field, as last drawn. */
    boolean back() {
        if (field.getText().length() == 0) {
            return false;
        }
        field.setText("");
        return true;
    }

    /** A finger on the list (walk 3): the library holds its order, and so does the pane. */
    private void finger(MotionEvent event) {
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN) {
            rows.touched(SystemClock.uptimeMillis());
            rows.press(list.pointToPosition((int) event.getX(), (int) event.getY()));
            rows.down();
            verb("touch", "", "", 1, false, false);
        } else if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            rows.touched(SystemClock.uptimeMillis());
            if (action == MotionEvent.ACTION_CANCEL) {
                rows.release();
            }
            int top = topOfFirst();
            int said = rows.up(firstOnScreen(), list.getLastVisiblePosition());
            if (said == ExploreRows.SHOWN) {
                place(top);
            } else if (said == ExploreRows.WAITING) {
                // The rows on screen took the waiting answer's words (1639).
                rebind();
            }
            chip();
            restLater();
            // A click owed that never comes still lets what waits through.
            list.removeCallbacks(settleLater);
            list.postDelayed(settleLater, ViewConfiguration.getPressedStateDuration() + 200);
            verb("touch", "", "", 0);
        }
    }

    /** The first row on screen, or -1 while the list shows none. */
    private int firstOnScreen() {
        return list.getChildCount() == 0 ? -1 : list.getFirstVisiblePosition();
    }

    /** The pixel the first row on screen stands at. */
    private int topOfFirst() {
        View first = list.getChildAt(0);
        return first == null ? 0 : first.getTop();
    }

    /**
     * The rows on screen take their rows' newest words, each view drawn
     * again where it stands, with no word to the adapter (1653, walk 8,
     * 39): {@code notifyDataSetChanged} sets ListView's
     * {@code mDataChanged}, and a click that meets it is dropped. A
     * waiting answer changes words only ({@link ExploreRows#notifies}), so
     * every position keeps its row and its view type.
     */
    private void rebind() {
        int first = list.getFirstVisiblePosition();
        int count = adapter.getCount();
        for (int i = 0; i < list.getChildCount(); i++) {
            int position = first + i;
            if (position < count) {
                adapter.getView(position, list.getChildAt(i), list);
            }
        }
    }

    /** The rows shown: the first row on screen back at its pixel, where the rule says. */
    private void place(int top) {
        adapter.notifyDataSetChanged();
        int anchor = rows.anchor();
        if (anchor >= 0) {
            list.setSelectionFromTop(anchor, ExploreRows.fromTop(top, list.getPaddingTop()));
        }
    }

    /**
     * The list's top band follows the chip (1639, walk 5 new 18: scrolled,
     * the chip still covered the top row's name). Without it, 20 dp the
     * rows scroll through; with it, the chip's lower half and 8 dp under
     * it, and the rows clipped there, so no row's words stand under the
     * chip. The rows on screen stay on their pixels either way.
     */
    private void band(boolean chip) {
        int want = chip ? Math.max(dp(28), fresh.getHeight() / 2 + dp(8)) : dp(20);
        if (list.getPaddingTop() == want && list.getClipToPadding() == chip) {
            return;
        }
        int first = firstOnScreen();
        int top = topOfFirst();
        list.setPadding(0, want, 0, 0);
        list.setClipToPadding(chip);
        measureBand();
        if (first >= 0) {
            list.setSelectionFromTop(first, ExploreRows.fromTop(top, want));
        }
    }

    /**
     * ListView reads its padding into the band {@code setSelectionFromTop}
     * counts from ({@code mListPadding}) only when it is measured (1653,
     * walk 8, 38: after a tap on the chip "Seiten" stood 20 dp low, the
     * selection set against the chip's deeper band, which had gone). A
     * measure at the size it has makes the new band the one the next
     * selection counts from.
     */
    private void measureBand() {
        int width = list.getWidth();
        int height = list.getHeight();
        if (width > 0 && height > 0) {
            list.measure(View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY),
                    View.MeasureSpec.makeMeasureSpec(height, View.MeasureSpec.EXACTLY));
        }
    }

    /** The chip: how many new pages wait, or gone. */
    private void chip() {
        if (!rows.waits()) {
            fresh.setVisibility(View.GONE);
            band(false);
            return;
        }
        int n = rows.fresh();
        String words = n == 1 ? screen.getString(R.string.explore_fresh_one)
                : n > 1 ? screen.getString(R.string.explore_fresh_many, n)
                : screen.getString(R.string.explore_fresh_order);
        fresh.setText(words);
        fresh.setVisibility(View.VISIBLE);
        band(true);
    }

    private void tapped(Row row) {
        switch (row.kind) {
            case Row.PAGE:
                opening = row.words;
                verb("open", "", Core.str(row.item, "dest"), 0);
                break;
            case Row.ADDRESS:
                verb("address", "", "", 0);
                break;
            case Row.ROOM:
                verb("room", "", "", row.index);
                break;
            case Row.SERVER:
                verb("hub", "", Core.str(row.item, "dest"), 0);
                break;
            case Row.OPEN:
                verb("rooms", "", "", 0);
                break;
            default:
                break;
        }
    }

    /** A page's menu (walk 9): bookmark or not, open, copy its address. */
    private void menu(View anchor, final Map<String, Object> page) {
        final String dest = Core.str(page, "dest");
        PopupMenu popup = new PopupMenu(screen, anchor);
        Menu items = popup.getMenu();
        boolean marked = Core.bool(page, "bookmarked");
        items.add(0, R.string.explore_open, 0, R.string.explore_open);
        items.add(0, R.string.explore_bookmark, 1,
                marked ? R.string.explore_unbookmark : R.string.explore_bookmark);
        items.add(0, R.string.explore_copy_address, 2, R.string.explore_copy_address);
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                int id = item.getItemId();
                if (id == R.string.explore_open) {
                    opening = Core.str(page, "label");
                    verb("open", "", dest, 0);
                } else if (id == R.string.explore_bookmark) {
                    forced = true;
                    verb("bookmark", "", dest, 0);
                } else if (id == R.string.explore_copy_address) {
                    // The address, as the menu says (1621, walk 2 new 7):
                    // the node's 32 hex, no page path.
                    ClipboardManager clip =
                            (ClipboardManager) screen.getSystemService(Context.CLIPBOARD_SERVICE);
                    clip.setPrimaryClip(ClipData.newPlainText("address", dest));
                    Toast.makeText(screen, R.string.explore_copied, Toast.LENGTH_SHORT).show();
                }
                return true;
            }
        });
        popup.show();
    }

    private void verb(String verb, String text, String dest, int index) {
        verb(verb, text, dest, index, false, true);
    }

    private void verb(String verb, String text, String dest, int index, boolean typing) {
        verb(verb, text, dest, index, typing, true);
    }

    /** A verb on the engine's thread (1446); the tab drawn after it where `drawn`. */
    private void verb(final String verb, final String text, final String dest, final int index,
            final boolean typing, final boolean drawn) {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.explore(verb, text, dest, index);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                if (typing) {
                    inFlight--;
                }
                if (error != null) {
                    Toast.makeText(screen, error, Toast.LENGTH_SHORT).show();
                }
                if (drawn) {
                    draw();
                }
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
            closeSheet();
            host.page(opening);
            opening = "";
            return;
        }
        if (showing.equals("rooms")) {
            closeSheet();
            host.rooms();
            return;
        }
        if (!showing.equals("list")) {
            closeSheet();
            host.left();
            return;
        }
        String query = Core.str(shown, "query");
        if (ExploreRows.echo(query, inFlight, field.getText().toString())) {
            typed = query;
            field.setText(query);
            field.setSelection(query.length());
        }
        field.setHint(Core.str(shown, "placeholder"));
        sort.setText(Core.str(shown, "sort"));
        boolean searched = drawnFor != null && !drawnFor.equals(query);
        boolean own = forced || searched;
        forced = false;
        int top = topOfFirst();
        int said;
        if (returning && !own) {
            said = rows.back(flatten(shown), firstOnScreen(), list.getLastVisiblePosition(),
                    atTop(), SystemClock.uptimeMillis());
        } else {
            said = rows.offer(flatten(shown), firstOnScreen(), list.getLastVisiblePosition(), own,
                    atTop(), SystemClock.uptimeMillis());
        }
        returning = false;
        for (int i = 0; i < jump.length; i++) {
            String words = Core.str(shown, JUMP_WORDS[i]);
            jump[i].setText(words);
            jump[i].setVisibility(words.isEmpty() ? View.GONE : View.VISIBLE);
        }
        if (said == ExploreRows.SHOWN) {
            place(top);
        } else if (said == ExploreRows.WAITING) {
            // Nothing moves, but the rows on screen say what the answer
            // says (1639, walk 5 new 26: a server row older than its sheet).
            rebind();
        }
        chip();
        if (searched) {
            list.setSelection(0);
        }
        drawnFor = query;
        sheet(shown);
    }

    /** The answer as the list's rows, in its order. */
    private List<Row> flatten(Map<String, Object> shown) {
        List<Row> out = new ArrayList<Row>();
        // How old the mailbox's list is, on top (1644).
        note(out, "as_of", Core.str(shown, "as_of"));
        out.add(new Row(Row.TITLE, "title:pages", Core.str(shown, "pages_title"), null, 0));
        note(out, "off", Core.str(shown, "pages_off"));
        String address = Core.str(shown, "address");
        if (!address.isEmpty()) {
            out.add(new Row(Row.ADDRESS, "address", address, null, 0));
        }
        for (Map<String, Object> page : Core.list(shown, "pages")) {
            String heading = Core.str(page, "heading");
            if (!heading.isEmpty()) {
                out.add(new Row(Row.HEADING, "heading:" + heading, heading, null, 0));
            }
            out.add(new Row(Row.PAGE, "page:" + Core.str(page, "dest"), Core.str(page, "label"), page, 0));
        }
        note(out, "pages_empty", Core.str(shown, "pages_empty"));
        // "Räume": the rooms the reader is in, or one sentence; then
        // "Server" with every server, or one sentence (1621).
        out.add(new Row(Row.TITLE, "title:rooms", Core.str(shown, "rooms_title"), null, 0));
        List<Map<String, Object>> joined = Core.list(shown, "joined");
        for (int i = 0; i < joined.size(); i++) {
            Map<String, Object> room = joined.get(i);
            out.add(new Row(Row.ROOM, "room:" + Core.str(room, "name"), Core.str(room, "name"), room, i));
        }
        note(out, "joined_empty", Core.str(shown, "joined_empty"));
        // The rooms screen of the server in use, joining nothing (1655).
        String open = Core.str(shown, "open_rooms");
        if (!open.isEmpty()) {
            out.add(new Row(Row.OPEN, "open:rooms", open, null, 0));
        }
        out.add(new Row(Row.HEADING, "heading:servers", Core.str(shown, "servers_title"), null, 0));
        for (Map<String, Object> hub : Core.list(shown, "hubs")) {
            out.add(new Row(Row.SERVER, "server:" + Core.str(hub, "dest"), Core.str(hub, "name"), hub, 0));
        }
        note(out, "servers_empty", Core.str(shown, "servers_empty"));
        return out;
    }

    private static void note(List<Row> out, String key, String words) {
        if (!words.isEmpty()) {
            out.add(new Row(Row.NOTE, "note:" + key, words, null, 0));
        }
    }

    /**
     * A server's sheet (walk 10): its name, when it was heard and how far,
     * the one button that makes it the public rooms' server with what that
     * tells the server, and the rooms it named, a tap joining one. Opening
     * it changes nothing; closing it says so to the library.
     */
    private void sheet(Map<String, Object> shown) {
        final String dest = Core.str(shown, "sheet_dest");
        if (dest.isEmpty()) {
            closeSheet();
            return;
        }
        String shows = Core.str(shown, "sheet_said") + "|" + Core.bool(shown, "sheet_chosen") + "|"
                + Core.words(shown, "sheet_rooms") + "|" + Core.str(shown, "sheet_rooms_note") + "|"
                + Core.str(shown, "sheet_retry") + "|" + Core.str(shown, "sheet_open_rooms");
        // While the question is out the sentence turns at its time, 20 s
        // (1653, walk 8, 42), which no answer may come to draw: once a
        // second, as the Rooms pane counts its seconds.
        list.removeCallbacks(askingTick);
        if (Core.bool(shown, "sheet_asking")) {
            list.postDelayed(askingTick, 1000);
        }
        if (sheet != null && sheet.isShowing() && dest.equals(sheetFor)) {
            // The server's answer came (1621): the same sheet, its rooms now.
            if (!shows.equals(sheetShows)) {
                sheetShows = shows;
                sheetBody.removeAllViews();
                fill(sheetBody, shown, dest);
            }
            return;
        }
        closeSheet();
        sheetFor = dest;
        sheetShows = shows;
        LinearLayout body = new LinearLayout(screen);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(dp(24), dp(4), dp(24), dp(8));
        sheetBody = body;
        fill(body, shown, dest);
        ScrollView scroll = new ScrollView(screen);
        scroll.addView(body);
        sheet = new AlertDialog.Builder(screen)
                .setTitle(Core.str(shown, "sheet_name"))
                .setView(scroll)
                .setNegativeButton(Core.str(shown, "sheet_close"), null)
                .create();
        sheet.setOnDismissListener(new DialogInterface.OnDismissListener() {
            @Override
            public void onDismiss(DialogInterface dialog) {
                // Closed by the reader: the library closes it too.
                if (dest.equals(sheetFor)) {
                    sheetFor = "";
                    verb("close", "", "", 0);
                }
            }
        });
        sheet.show();
    }

    /** The sheet's body: when it was heard, the button, its rooms and their sentence. */
    private void fill(LinearLayout body, Map<String, Object> shown, final String dest) {
        String said = Core.str(shown, "sheet_said");
        if (!said.isEmpty()) {
            body.addView(words(said, 14, R.color.text_secondary));
        }
        if (Core.bool(shown, "sheet_chosen")) {
            TextView inUse = words(Core.str(shown, "sheet_button"), 16, R.color.text);
            inUse.setPadding(0, dp(12), 0, dp(4));
            body.addView(inUse);
            // Its rooms screen, joining nothing (1655, walk 9, 45).
            String open = Core.str(shown, "sheet_open_rooms");
            if (!open.isEmpty()) {
                Button rooms = new Button(screen);
                rooms.setText(open);
                rooms.setAllCaps(false);
                rooms.setTag("explore_sheet_open_rooms");
                rooms.setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        sheetFor = "";
                        closeSheet();
                        verb("rooms", "", "", 0);
                    }
                });
                body.addView(rooms);
            }
        } else {
            Button use = new Button(screen);
            use.setText(Core.str(shown, "sheet_button"));
            use.setAllCaps(false);
            use.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    sheetFor = "";
                    closeSheet();
                    verb("use", "", dest, 0);
                }
            });
            body.addView(use);
            body.addView(words(Core.str(shown, "sheet_notice"), 13, R.color.text_secondary));
        }
        TextView title = words(Core.str(shown, "sheet_rooms_title"), 16, R.color.text);
        title.setTypeface(null, Typeface.BOLD);
        title.setPadding(0, dp(16), 0, dp(4));
        body.addView(title);
        for (final String room : Core.words(shown, "sheet_rooms")) {
            TextView join = words(room, 17, R.color.accent);
            join.setPadding(0, dp(10), 0, dp(10));
            join.setBackgroundResource(android.R.drawable.list_selector_background);
            join.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    sheetFor = "";
                    closeSheet();
                    verb("join", room, dest, 0);
                }
            });
            body.addView(join);
        }
        String note = Core.str(shown, "sheet_rooms_note");
        if (!note.isEmpty()) {
            TextView line = words(note, 14, R.color.text_secondary);
            line.setTag("explore_sheet_note");
            body.addView(line);
        }
        String retry = Core.str(shown, "sheet_retry");
        if (!retry.isEmpty()) {
            TextView again = words(retry, 16, R.color.accent);
            again.setPadding(0, dp(10), 0, dp(10));
            again.setTag("explore_sheet_retry");
            again.setBackgroundResource(android.R.drawable.list_selector_background);
            again.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    verb("ask", "", dest, 0);
                }
            });
            body.addView(again);
        }
    }

    /** The sheet's question still out: draw again, its sentence may turn (1653). */
    private final Runnable askingTick = new Runnable() {
        @Override
        public void run() {
            if (sheet != null && sheet.isShowing() && !screen.isDestroyed()) {
                draw();
            }
        }
    };

    /** The sheet off the screen, by the library's word: no verb back. */
    private void closeSheet() {
        if (sheet != null) {
            sheetFor = "";
            if (sheet.isShowing()) {
                sheet.dismiss();
            }
            sheet = null;
        }
    }

    /** The list's rows: only the visible ones are views, each by its stable id. */
    private final class Adapter extends BaseAdapter {
        @Override
        public int getCount() {
            return rows.size();
        }

        @Override
        public Object getItem(int position) {
            return rows.at(position);
        }

        @Override
        public long getItemId(int position) {
            return ExploreRows.id(rows.at(position).key);
        }

        @Override
        public boolean hasStableIds() {
            return true;
        }

        @Override
        public int getViewTypeCount() {
            return Row.KINDS;
        }

        @Override
        public int getItemViewType(int position) {
            return rows.at(position).kind;
        }

        @Override
        public boolean isEnabled(int position) {
            int kind = rows.at(position).kind;
            return kind == Row.ADDRESS || kind == Row.PAGE || kind == Row.ROOM || kind == Row.SERVER
                    || kind == Row.OPEN;
        }

        @Override
        public View getView(int position, View convert, ViewGroup parent) {
            Row row = rows.at(position);
            switch (row.kind) {
                case Row.TITLE:
                    return title(convert, row.words);
                case Row.NOTE:
                    return note(convert, row.words);
                case Row.HEADING:
                    return heading(convert, row.words);
                case Row.ADDRESS:
                    return address(convert, row.words);
                case Row.OPEN:
                    return openRooms(convert, row.words);
                case Row.PAGE:
                    return item(convert, row, Core.str(row.item, "said"), true);
                case Row.ROOM:
                    return item(convert, row, room(row.item), false);
                default:
                    return item(convert, row, Core.str(row.item, "value"), false);
            }
        }
    }

    /** A room's line under its name: the globe and its word, the unread. */
    private static String room(Map<String, Object> room) {
        long unread = Core.num(room, "unread");
        return "🌐 " + Core.str(room, "public") + (unread > 0 ? " · " + unread : "");
    }

    /**
     * A page's, a room's or a server's row, one type for all (walk 13): the
     * name over its line, the star on a page.
     */
    private View item(View convert, Row row, String said, boolean starred) {
        LinearLayout view = convert instanceof LinearLayout ? (LinearLayout) convert : null;
        if (view == null) {
            view = new LinearLayout(screen);
            view.setGravity(Gravity.CENTER_VERTICAL);
            view.setPadding(dp(16), dp(8), dp(8), dp(8));
            view.setMinimumHeight(dp(56));
            LinearLayout words = new LinearLayout(screen);
            words.setOrientation(LinearLayout.VERTICAL);
            TextView name = new TextView(screen);
            name.setTextSize(17);
            name.setSingleLine(true);
            name.setEllipsize(TextUtils.TruncateAt.END);
            name.setTextColor(screen.getColor(R.color.text));
            words.addView(name);
            TextView line = new TextView(screen);
            line.setTextSize(13);
            line.setSingleLine(true);
            line.setEllipsize(TextUtils.TruncateAt.END);
            line.setTextColor(screen.getColor(R.color.text_secondary));
            words.addView(line);
            view.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
            TextView star = new TextView(screen);
            star.setTextSize(22);
            star.setPadding(dp(16), dp(8), dp(8), dp(8));
            star.setTextColor(screen.getColor(R.color.accent));
            view.addView(star);
        }
        LinearLayout words = (LinearLayout) view.getChildAt(0);
        TextView name = (TextView) words.getChildAt(0);
        TextView line = (TextView) words.getChildAt(1);
        TextView star = (TextView) view.getChildAt(1);
        boolean bold = row.kind == Row.ROOM && Core.num(row.item, "unread") > 0;
        boolean chosen = row.kind == Row.SERVER && Core.bool(row.item, "chosen");
        name.setText(row.words + (chosen ? " ✓" : ""));
        name.setTypeface(null, bold ? Typeface.BOLD : Typeface.NORMAL);
        line.setText(said);
        line.setVisibility(said.isEmpty() ? View.GONE : View.VISIBLE);
        view.setTag(tag(row));
        if (starred) {
            final String dest = Core.str(row.item, "dest");
            star.setVisibility(View.VISIBLE);
            star.setText(Core.bool(row.item, "bookmarked") ? "★" : "☆");
            star.setTag("explore_star_" + dest);
            star.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    verb("bookmark", "", dest, 0);
                }
            });
        } else {
            star.setVisibility(View.GONE);
            star.setOnClickListener(null);
            star.setClickable(false);
        }
        return view;
    }

    /** The tags the project walk finds rows by. */
    private static String tag(Row row) {
        switch (row.kind) {
            case Row.PAGE:
                return "explore_page_" + Core.str(row.item, "dest");
            case Row.ROOM:
                return "explore_room_" + row.index;
            case Row.SERVER:
                return "explore_hub_" + Core.str(row.item, "dest");
            default:
                return null;
        }
    }

    private TextView address(View convert, String words) {
        TextView view = reuse(convert, 17, R.color.accent);
        view.setPadding(dp(16), dp(12), dp(16), dp(12));
        view.setId(R.id.explore_address);
        view.setText(words);
        return view;
    }

    /** "Räume öffnen" (1655): an action in the accent, as the address is. */
    private TextView openRooms(View convert, String words) {
        TextView view = reuse(convert, 17, R.color.accent);
        view.setPadding(dp(16), dp(12), dp(16), dp(12));
        view.setTag("explore_open_rooms");
        view.setText(words);
        return view;
    }

    private TextView title(View convert, String words) {
        TextView view = reuse(convert, 18, R.color.text);
        view.setTypeface(null, Typeface.BOLD);
        view.setPadding(dp(16), dp(16), dp(16), dp(4));
        view.setText(words);
        return view;
    }

    private TextView heading(View convert, String words) {
        TextView view = reuse(convert, 13, R.color.text_secondary);
        view.setPadding(dp(16), dp(10), dp(16), dp(2));
        view.setText(words);
        return view;
    }

    private TextView note(View convert, String words) {
        TextView view = reuse(convert, 13, R.color.text_secondary);
        view.setPadding(dp(16), dp(8), dp(16), dp(8));
        view.setText(words);
        return view;
    }

    private TextView reuse(View convert, int size, int colour) {
        TextView view = convert instanceof TextView ? (TextView) convert : new TextView(screen);
        view.setTextSize(size);
        view.setTextColor(screen.getColor(colour));
        return view;
    }

    private TextView words(String text, int size, int colour) {
        TextView view = new TextView(screen);
        view.setText(text);
        view.setTextSize(size);
        view.setTextColor(screen.getColor(colour));
        return view;
    }

    private int dp(int value) {
        return (int) (value * screen.getResources().getDisplayMetrics().density + 0.5f);
    }
}
