package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Typeface;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.InputType;
import android.text.TextPaint;
import android.text.TextUtils;
import android.text.method.LinkMovementMethod;
import android.text.style.BackgroundColorSpan;
import android.text.style.ClickableSpan;
import android.text.style.ForegroundColorSpan;
import android.text.style.StyleSpan;
import android.text.style.UnderlineSpan;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.PopupMenu;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The Browse tab (1337): lnomad's browser hosted in Loquax. Every word,
 * every run of the page and every row is the library's
 * ({@link Core#browseScreen}); every tap is one verb ({@link Core#browse})
 * naming the scene node it landed on. The page arrives laid out, a window
 * of lines of runs, and is drawn in a monospace grid: nothing here parses
 * micron or knows a link from its text.
 *
 * <p>1621 (walk 2): the page goes by its name, in the screen's title and
 * in the address field, from the moment it is opened; the field shows the
 * node's address only while it is edited. Back is always there: at the
 * first page it gives the tab back to Explore.
 *
 * <p>1650: the page comes whole (up to the library's cap) and scrolls as
 * any phone list does; the arrows are there only for a page past the cap,
 * which comes as a window. The finger's line goes back as {@code at}, so
 * a fresh copy, Back and the arrows start where the reader is. The bar's
 * actions are a menu drawn from the library's list, its chips a line
 * under the bar, and a field opens the editor its kind asks for.
 */
final class BrowsePane {
    /** What the pane hands back to the Activity. */
    interface Host {
        /** The tab gave way to a chat (a page's link to its operator), or to Explore. */
        void left();

        /** The page's name, for the screen's title. */
        void titled(String title);
    }

    private final Activity screen;
    private final Host host;
    private final ImageButton back;
    private final EditText address;
    private final TextView page;
    private final ScrollView pageScroll;
    /**
     * The page's sideways scroll (1667, walk 11, finding 60): a line of text
     * art wider than the phone comes whole from the library and is scrolled
     * across, as a terminal wider than the screen would show it, never cut
     * at the right edge or broken.
     */
    private final android.widget.HorizontalScrollView pageSideways;
    /** The bar's state under it: the key while identifying, cache, hops (1650). */
    private final TextView chips;
    private final View foot;
    private final Button pageUp;
    private final Button pageDown;
    private final LinearLayout rows;
    /**
     * The places sheet's own scroll, a layer over the page (1639, walk 5
     * new 23): sheet and page shared one scroll, so past the sheet's last
     * entry the page followed and the sheet counted as open while it was
     * off screen. Shown exactly while the library says a sheet is open.
     */
    private final ScrollView sheetScroll;
    private final TextView status;
    /** The loading state over an empty page (1632, walk 4 new 12). */
    private final LinearLayout loadingState;
    private final TextView loadingWords;
    private int cols;
    /** The cells said again once for a page laid out wider: never a loop. */
    private int resentAt = -1;
    private int lines;

    BrowsePane(Activity activity, Host host) {
        this.screen = activity;
        this.host = host;
        FrameLayout pane = (FrameLayout) activity.findViewById(R.id.pane_browse);
        LinearLayout column = new LinearLayout(activity);
        column.setOrientation(LinearLayout.VERTICAL);
        pane.addView(column, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        LinearLayout bar = new LinearLayout(activity);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        back = new ImageButton(activity);
        back.setBackground(null);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, activity.getColor(R.color.text)));
        back.setContentDescription(activity.getString(R.string.back));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("back", "", "");
            }
        });
        // A glyph has no size of its own: the button gives it one (walk 2:
        // a 61 px button with nothing drawn in it).
        back.setTag("browse_back");
        bar.addView(back, new LinearLayout.LayoutParams(dp(48), dp(48)));
        address = new EditText(activity);
        address.setSingleLine(true);
        // An address is no word: no spelling marks under it (walk 2).
        address.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI
                | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        address.setImeOptions(EditorInfo.IME_ACTION_GO);
        address.setTag("address");
        address.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                String typed = v.getText().toString().trim();
                // The field still says the node's own address: the page
                // again, unless it is on its way already (1629, walk 3: a
                // page that never came could not be asked for again).
                if (!typed.isEmpty() && !typed.equals(Core.str(last, "dest"))) {
                    verb("go", "", typed);
                } else if (!typed.isEmpty() && !Core.bool(last, "loading")) {
                    verb("reload", "", "");
                }
                address.clearFocus();
                // The load starts: the keyboard goes, as opening a page
                // from the search does (1632, walk 4 new 13).
                InputMethodManager keyboard =
                        (InputMethodManager) screen.getSystemService(Context.INPUT_METHOD_SERVICE);
                if (keyboard != null) {
                    keyboard.hideSoftInputFromWindow(address.getWindowToken(), 0);
                }
                return true;
            }
        });
        // Edited, the field holds the node's address, chosen whole so a
        // typed one replaces it; left, the page's name again.
        address.setOnFocusChangeListener(new View.OnFocusChangeListener() {
            @Override
            public void onFocusChange(View v, boolean focused) {
                if (last == null) {
                    return;
                }
                if (focused) {
                    String dest = Core.str(last, "dest");
                    address.setText(dest);
                    address.selectAll();
                } else {
                    address.setText(Core.str(last, "address"));
                }
            }
        });
        bar.addView(address, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button places = new Button(activity);
        // A word, not a grey "☰" (1629, walk 3 new 7).
        places.setText(R.string.browse_places);
        places.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("places", "", "");
            }
        });
        bar.addView(places);
        // The library's actions, drawn as it lists them (1650): no word
        // and no verb here of Java's own.
        ImageButton more = new ImageButton(activity);
        more.setBackground(null);
        more.setImageDrawable(new Glyph(Glyph.Kind.MENU, activity.getColor(R.color.text)));
        more.setContentDescription(activity.getString(R.string.menu));
        more.setTag("browse_menu");
        more.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                actions(v);
            }
        });
        bar.addView(more, new LinearLayout.LayoutParams(dp(48), dp(48)));
        column.addView(bar);
        chips = new TextView(activity);
        chips.setTextColor(activity.getColor(R.color.text_secondary));
        chips.setSingleLine(true);
        chips.setEllipsize(TextUtils.TruncateAt.END);
        chips.setPadding(dp(16), 0, dp(16), dp(4));
        chips.setTag("browse_chips");
        chips.setVisibility(View.GONE);
        column.addView(chips);

        final ScrollView scroll = new ScrollView(activity);
        pageScroll = scroll;
        scroll.setTag("browse_page_scroll");
        LinearLayout body = new LinearLayout(activity);
        body.setOrientation(LinearLayout.VERTICAL);
        rows = new LinearLayout(activity);
        rows.setOrientation(LinearLayout.VERTICAL);
        sheetScroll = new ScrollView(activity);
        sheetScroll.setTag("browse_sheet");
        sheetScroll.setBackgroundColor(activity.getColor(R.color.background));
        sheetScroll.setFillViewport(true);
        sheetScroll.addView(rows);
        sheetScroll.setVisibility(View.GONE);
        page = new TextView(activity);
        page.setTypeface(Typeface.MONOSPACE);
        page.setTextSize(13);
        page.setTextColor(activity.getColor(R.color.text));
        page.setMovementMethod(LinkMovementMethod.getInstance());
        page.setTag("page");
        // A gutter on both sides (1632, walk 4 new 16: the text stood at
        // x 0), and one wrap: the library lays the page out on the cells
        // the view holds ({@link #size}); the view breaks no line again,
        // which turned each long line into a long and a short one.
        page.setPadding(dp(16), dp(8), dp(16), dp(8));
        page.setHorizontallyScrolling(true);
        pageSideways = new android.widget.HorizontalScrollView(activity);
        pageSideways.setTag("browse_page_sideways");
        pageSideways.setFillViewport(true);
        pageSideways.setHorizontalScrollBarEnabled(false);
        pageSideways.addView(page, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        body.addView(pageSideways, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        scroll.addView(body);
        FrameLayout stage = new FrameLayout(activity);
        stage.addView(scroll, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        stage.addView(sheetScroll, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        // While a page with nothing to show yet is on its way: a spinner
        // and "Lade <name> …" in the middle, not a black screen with a
        // grey line in its corner (1632, walk 4 new 12).
        loadingState = new LinearLayout(activity);
        loadingState.setOrientation(LinearLayout.VERTICAL);
        loadingState.setGravity(Gravity.CENTER);
        loadingState.setBackgroundColor(activity.getColor(R.color.background));
        loadingState.setTag("browse_loading");
        loadingState.setClickable(true);
        ProgressBar spinner = new ProgressBar(activity);
        spinner.setIndeterminate(true);
        loadingState.addView(spinner, new LinearLayout.LayoutParams(dp(48), dp(48)));
        loadingWords = new TextView(activity);
        loadingWords.setTextSize(16);
        loadingWords.setTextColor(activity.getColor(R.color.text));
        loadingWords.setGravity(Gravity.CENTER);
        loadingWords.setPadding(dp(24), dp(16), dp(24), 0);
        loadingState.addView(loadingWords, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        loadingState.setVisibility(View.GONE);
        stage.addView(loadingState, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        column.addView(stage, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout foot = new LinearLayout(activity);
        this.foot = foot;
        foot.setGravity(Gravity.CENTER_VERTICAL);
        status = new TextView(activity);
        status.setTextColor(activity.getColor(R.color.text_secondary));
        // One line in a frame, cut with an ellipsis (1632, walk 4 new 12:
        // a long name wrapped it, and it stood without an edge).
        status.setSingleLine(true);
        status.setEllipsize(TextUtils.TruncateAt.END);
        status.setPadding(dp(12), dp(6), dp(12), dp(6));
        android.graphics.drawable.GradientDrawable frame =
                new android.graphics.drawable.GradientDrawable();
        frame.setCornerRadius(dp(12));
        frame.setStroke(dp(1), activity.getColor(R.color.text_secondary));
        status.setBackground(frame);
        LinearLayout.LayoutParams statusAt =
                new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1);
        statusAt.setMargins(dp(8), dp(4), dp(4), dp(4));
        foot.addView(status, statusAt);
        pageUp = scrollButton("▲", "pageup", R.string.browse_page_up);
        pageDown = scrollButton("▼", "pagedown", R.string.browse_page_down);
        foot.addView(pageUp);
        foot.addView(pageDown);
        column.addView(foot);

        // The finger's line goes back once the page holds still (1650).
        scroll.setOnScrollChangeListener(new View.OnScrollChangeListener() {
            @Override
            public void onScrollChange(View v, int x, int y, int ox, int oy) {
                page.removeCallbacks(report);
                page.postDelayed(report, 250);
            }
        });

        // The page is laid out on the cells the view holds.
        page.addOnLayoutChangeListener(new View.OnLayoutChangeListener() {
            @Override
            public void onLayoutChange(View v, int l, int t, int r, int b, int ol, int ot, int or, int ob) {
                size();
            }
        });
    }

    private Button scrollButton(String label, final String how, int words) {
        Button button = new Button(screen);
        button.setText(label);
        // An arrow that says what it does, not a grey box (1629, walk 3 new 7).
        button.setContentDescription(screen.getString(words));
        button.setBackground(null);
        button.setTextColor(screen.getColor(R.color.accent));
        button.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // With the sheet up the arrows scroll the sheet, as the
                // finger does; the page under it stays (1639).
                if (sheetScroll.getVisibility() == View.VISIBLE) {
                    sheetScroll.pageScroll("pageup".equals(how) ? View.FOCUS_UP : View.FOCUS_DOWN);
                    return;
                }
                verb("scroll", "", how);
            }
        });
        return button;
    }

    /** The page view's top in the vertical scroll's content. */
    private int pageTop() {
        return pageSideways.getTop() + page.getTop();
    }

    /** The page line at the top of the view, absolute, or -1 before a layout. */
    private int topLine() {
        int height = page.getLineHeight();
        if (height <= 0 || last == null) {
            return -1;
        }
        int y = pageScroll.getScrollY() - pageTop() - page.getPaddingTop();
        return (int) Core.num(last, "first") + Math.max(0, y) / height;
    }

    /** The line the finger left the page at, told to the library. */
    private final Runnable report = new Runnable() {
        @Override
        public void run() {
            final int line = topLine();
            if (line < 0 || line == reported || line == shownAt) {
                return;
            }
            reported = line;
            final boolean windowed = windowed(last);
            Engine.run(new Engine.Call() {
                @Override
                public String run() {
                    return Core.browse("at", "", String.valueOf(line));
                }
            }, new Engine.Said() {
                @Override
                public void said(String error) {
                    // A whole page needs nothing drawn: the view is where
                    // the library now says it is. A window moves with the line.
                    if (windowed) {
                        draw();
                    }
                }
            });
        }
    };

    /** The line Java last told the library, and the line the library last said. */
    private int reported = -1;
    private int shownAt = -1;
    /** The page's lines as last drawn: the same lines are not drawn again. */
    private List<Map<String, Object>> drawn;
    private String drawnKey = "";
    /** The page's fields by node (1650): which editor a tap opens. */
    private final Map<String, Map<String, Object>> fields =
            new HashMap<String, Map<String, Object>>();
    private String focus = "";

    /** A page longer than the library hands whole: a window, and the arrows. */
    private static boolean windowed(Map<String, Object> shown) {
        return shown != null
                && Core.num(shown, "total") > Core.list(shown, "lines").size();
    }

    /** The bar's menu: the library's actions, in its words, as it enables them. */
    private void actions(View anchor) {
        if (last == null) {
            return;
        }
        final List<Map<String, Object>> list = Core.list(last, "actions");
        PopupMenu popup = new PopupMenu(screen, anchor);
        Menu items = popup.getMenu();
        for (int i = 0; i < list.size(); i++) {
            Map<String, Object> action = list.get(i);
            MenuItem item = items.add(0, i, i, Core.str(action, "label"));
            item.setEnabled(Core.bool(action, "enabled"));
        }
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                int at = item.getItemId();
                if (at >= 0 && at < list.size()) {
                    verb(Core.str(list.get(at), "verb"), "", "");
                }
                return true;
            }
        });
        popup.show();
    }

    /** The tab came to the front. */
    void show() {
        show("");
    }

    /**
     * The tab came to the front for the page named {@code name} (empty
     * when not known): the loading state stands at once, before the
     * library's first answer (1632, walk 4 new 12: the first frame said
     * "Seiten" over an empty field).
     */
    void show(String name) {
        if (!name.isEmpty()) {
            if (!address.hasFocus()) {
                address.setText(name);
            }
            loadingWords.setText(screen.getString(R.string.browse_loading, name));
            loadingState.setVisibility(View.VISIBLE);
            page.setText("");
            drawn = null;
            drawnKey = "";
            status.setVisibility(View.GONE);
        }
        verb("tab", "", "");
    }

    /** Back: the browser's back while it has one, as last drawn; false when it has none. */
    boolean back() {
        if (last == null || !Core.bool(last, "back")) {
            return false;
        }
        verb("back", "", "");
        return true;
    }

    /** The cells of the page view, sent when they change. */
    private void size() {
        Paint paint = page.getPaint();
        float cell = paint.measureText("M");
        // The cells the screen shows, not the page's own width: inside the
        // sideways scroll the page is as wide as its widest line (1667).
        int width = pageSideways.getWidth() - page.getPaddingLeft() - page.getPaddingRight();
        View scroll = pageScroll;
        int c = cell > 0 ? (int) (width / cell) : 0;
        int r = page.getLineHeight() > 0 ? scroll.getHeight() / page.getLineHeight() : 0;
        if (c > 0 && r > 0 && (c != cols || r != lines)) {
            cols = c;
            lines = r;
            Engine.run(new Engine.Call() {
                @Override
                public String run() {
                    return Core.browseSize(c, r);
                }
            }, new Engine.Said() {
                @Override
                public void said(String error) {
                    draw();
                }
            });
        }
    }

    /** A verb on the engine's thread (1446); the tab drawn after it. */
    private void verb(final String verb, final String node, final String text) {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.browse(verb, node, text);
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

    /** What the tab showed last (1464): Back decides by it. */
    private Map<String, Object> last;

    /** Draw what the library says the tab shows, asked on the engine's thread (1464). */
    void draw() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.browseScreen();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                if (!screen.isDestroyed()) {
                    show(shown);
                }
            }
        });
    }

    private void show(Map<String, Object> shown) {
        last = shown;
        handOver(shown);
        if (!Core.bool(shown, "open")) {
            host.left();
            return;
        }
        back.setVisibility(Core.bool(shown, "back") ? View.VISIBLE : View.GONE);
        if (!address.hasFocus()) {
            address.setText(Core.str(shown, "address"));
        }
        host.titled(Core.str(shown, "title"));
        StringBuilder chipLine = new StringBuilder();
        for (String chip : Core.words(shown, "chips")) {
            if (chipLine.length() > 0) {
                chipLine.append(" · ");
            }
            chipLine.append(chip);
        }
        chips.setText(chipLine);
        chips.setVisibility(chipLine.length() == 0 ? View.GONE : View.VISIBLE);
        address.setHint(Core.str(shown, "address_hint"));
        String loading = Core.str(shown, "loading_words");
        loadingWords.setText(loading);
        loadingState.setVisibility(loading.isEmpty() ? View.GONE : View.VISIBLE);
        String said = Core.str(shown, "status");
        status.setText(said);
        // The status line says what the loading state does not: a cached
        // copy standing while the fresh one comes, a download.
        status.setVisibility(said.isEmpty() || !loading.isEmpty() ? View.INVISIBLE : View.VISIBLE);
        // The arrows only for a page past the cap, which comes as a window;
        // a whole page scrolls under the finger (1650).
        boolean windowed = windowed(shown);
        pageUp.setVisibility(windowed ? View.VISIBLE : View.GONE);
        pageDown.setVisibility(windowed ? View.VISIBLE : View.GONE);
        foot.setVisibility(windowed || !said.isEmpty() ? View.VISIBLE : View.GONE);
        // The seconds count while a page is on its way.
        page.removeCallbacks(redraw);
        if (Core.bool(shown, "loading")) {
            page.postDelayed(redraw, 1000);
        }
        String toast = Core.str(shown, "toast");
        if (!toast.isEmpty()) {
            Toast.makeText(screen, toast, Toast.LENGTH_SHORT).show();
        }

        rows.removeAllViews();
        String sheet = Core.str(shown, "sheet");
        if (!sheet.isEmpty()) {
            TextView title = new TextView(screen);
            title.setText(sheet);
            title.setTypeface(null, Typeface.BOLD);
            title.setTextColor(screen.getColor(R.color.text));
            title.setPadding(dp(16), dp(16), dp(16), dp(8));
            rows.addView(title);
        }
        // A line between two entries (1639, walk 5 new 24).
        boolean item = false;
        for (Map<String, Object> row : Core.list(shown, "rows")) {
            boolean next = "item".equals(Core.str(row, "kind"));
            if (item && next) {
                View line = new View(screen);
                line.setBackgroundColor(screen.getColor(R.color.bar));
                LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT, Math.max(1, dp(1)));
                at.setMargins(dp(16), 0, dp(16), 0);
                rows.addView(line, at);
            }
            item = next;
            rows.addView(row(row));
        }
        // The sheet is on screen exactly while the library has it open; a
        // sheet that opens starts at its top.
        boolean opens = !sheet.isEmpty() && sheetScroll.getVisibility() != View.VISIBLE;
        sheetScroll.setVisibility(sheet.isEmpty() ? View.GONE : View.VISIBLE);
        if (opens) {
            sheetScroll.scrollTo(0, 0);
        }

        fields.clear();
        for (Map<String, Object> field : Core.list(shown, "fields")) {
            fields.put(Core.str(field, "node"), field);
        }
        List<Map<String, Object>> lines = Core.list(shown, "lines");
        String focused = Core.str(shown, "focus");
        int at = (int) (Core.num(shown, "at") - Core.num(shown, "first"));
        boolean moved = Core.num(shown, "at") != shownAt && Core.num(shown, "at") != reported;
        shownAt = (int) Core.num(shown, "at");
        if (lines.equals(drawn) && focused.equals(focus)) {
            // The same page: the view stays where the finger has it, unless
            // the library moved the reader (an anchor, the arrows).
            if (moved) {
                scrollToLine(at);
            }
            return;
        }
        // A filled field or a moved focus redraws the same page; another
        // page (another node, title or length) starts where the library says.
        String key = Core.str(shown, "dest") + "|" + Core.str(shown, "title") + "|"
                + Core.num(shown, "total");
        boolean samePage = key.equals(drawnKey);
        drawnKey = key;
        drawn = lines;
        focus = focused;
        SpannableStringBuilder text = new SpannableStringBuilder();
        int widest = 0;
        for (Map<String, Object> line : lines) {
            int start = text.length();
            for (Map<String, Object> run : Core.list(line, "runs")) {
                append(text, run);
            }
            widest = Math.max(widest, text.length() - start);
            text.append('\n');
        }
        fitCells(text);
        page.setText(text);
        // Another page starts where the library says (its top, an anchor,
        // the line Back left it at); the same page redrawn for its focus
        // keeps the finger's place.
        if (!samePage || moved) {
            scrollToLine(at);
        }
        if (!samePage) {
            pageSideways.scrollTo(0, 0);
        }
        // A page laid out wider than the view holds was laid out on cells
        // the library never heard of this view (a size said before the
        // session ran): say them again.
        if (cols > 0 && widest > cols && resentAt != cols) {
            resentAt = cols;
            cols = 0;
            size();
        }
    }

    /** The view's top at line {@code line} of the lines on screen, once laid out. */
    private void scrollToLine(final int line) {
        page.post(new Runnable() {
            @Override
            public void run() {
                int y = pageTop() + page.getPaddingTop()
                        + Math.max(0, line) * page.getLineHeight();
                pageScroll.scrollTo(0, line <= 0 ? 0 : y);
            }
        });
    }

    /** What each character measures, in cells of the page's monospace font. */
    private final java.util.HashMap<Integer, Float> cellScale = new java.util.HashMap<Integer, Float>();

    /**
     * Every character drawn on the grid's cells (1639, walk 5 new 22: on
     * RU3AID the block graphics came from a wider fallback font and
     * "последние замеры" was cut on the right). The monospace font has no
     * glyph for some characters; the system draws them from another font,
     * at that font's width. A character that does not measure a whole
     * number of cells is scaled to the nearest one (at least one), so the
     * line stays as wide as the library laid it out; colours, background,
     * underline and links stay on it.
     */
    private void fitCells(SpannableStringBuilder text) {
        Paint paint = page.getPaint();
        float cell = paint.measureText("M");
        if (cell <= 0) {
            return;
        }
        int i = 0;
        while (i < text.length()) {
            int point = Character.codePointAt(text, i);
            int size = Character.charCount(point);
            if (point >= 0x80 && point != '\n') {
                Float scale = cellScale.get(point);
                if (scale == null) {
                    float width = paint.measureText(new String(Character.toChars(point)));
                    float cells = Math.max(1, Math.round(width / cell));
                    scale = width <= 0 ? 1f : cells * cell / width;
                    cellScale.put(point, scale);
                }
                if (Math.abs(scale - 1f) > 0.01f) {
                    text.setSpan(new android.text.style.ScaleXSpan(scale), i, i + size,
                            Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
                }
            }
            i += size;
        }
    }

    /** The next second of a load: the tab drawn again. */
    private final Runnable redraw = new Runnable() {
        @Override
        public void run() {
            if (!screen.isDestroyed() && last != null && Core.bool(last, "loading")) {
                draw();
            }
        }
    };

    /**
     * What the library hands out once with the screen (1337 pass two): a
     * text the browser copied goes to the clipboard, and a file it
     * downloaded, kept under the app's files as a bubble's is, opens in the
     * app the system has for its kind, or the share sheet takes it.
     */
    private void handOver(Map<String, Object> shown) {
        String copy = Core.str(shown, "copy");
        if (!copy.isEmpty()) {
            ClipboardManager clip = screen.getSystemService(ClipboardManager.class);
            if (clip != null) {
                clip.setPrimaryClip(ClipData.newPlainText("NomadNet", copy));
            }
        }
        String path = Core.str(shown, "file");
        if (path.isEmpty()) {
            return;
        }
        android.net.Uri uri = FilesProvider.uri(screen, path);
        if (uri == null) {
            Toast.makeText(screen, R.string.file_unreadable, Toast.LENGTH_SHORT).show();
            return;
        }
        String name = Core.str(shown, "file_name");
        String type = FilesProvider.type(name);
        Intent send = new Intent(Intent.ACTION_SEND)
                .setType(type)
                .putExtra(Intent.EXTRA_STREAM, uri)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        send.setClipData(ClipData.newRawUri(name, uri));
        Intent chooser = Intent.createChooser(send, name)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        Intent view = new Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, type)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        try {
            screen.startActivity(view);
        } catch (android.content.ActivityNotFoundException e) {
            try {
                screen.startActivity(chooser);
            } catch (android.content.ActivityNotFoundException again) {
                Toast.makeText(screen, R.string.file_no_app, Toast.LENGTH_SHORT).show();
            }
        }
    }

    private View row(Map<String, Object> row) {
        TextView view = new TextView(screen);
        view.setText(Core.str(row, "text"));
        view.setPadding(dp(16), dp(12), dp(16), dp(12));
        String kind = Core.str(row, "kind");
        final String node = Core.str(row, "node");
        if ("heading".equals(kind)) {
            view.setTypeface(null, Typeface.BOLD);
        } else if ("item".equals(kind)) {
            view.setTag(node);
            view.setBackgroundResource(android.R.drawable.list_selector_background);
            view.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    verb("activate", node, "");
                }
            });
        } else {
            view.setTextColor(screen.getColor(R.color.text_secondary));
        }
        return view;
    }

    /** One run as spans: its inks, weight, underline, and its node's tap. */
    private void append(SpannableStringBuilder text, Map<String, Object> run) {
        int start = text.length();
        text.append(Core.str(run, "text"));
        int end = text.length();
        int flags = Spanned.SPAN_EXCLUSIVE_EXCLUSIVE;
        final String node = Core.str(run, "node");
        // Reverse with the view's own inks swaps those (1650: a reversed
        // run without inks drew as plain text), and the focused node is
        // drawn as the terminal draws it, in the accent.
        boolean reverse = Core.bool(run, "reverse");
        Integer fg = ink(Core.str(run, reverse ? "bg" : "fg"));
        Integer bg = ink(Core.str(run, reverse ? "fg" : "bg"));
        if (reverse) {
            fg = fg != null ? fg : screen.getColor(R.color.background);
            bg = bg != null ? bg : screen.getColor(R.color.text);
        }
        if (!node.isEmpty() && node.equals(focus)) {
            fg = screen.getColor(R.color.background);
            bg = screen.getColor(R.color.accent);
        }
        if (fg != null) {
            text.setSpan(new ForegroundColorSpan(fg), start, end, flags);
        }
        if (bg != null) {
            text.setSpan(new BackgroundColorSpan(bg), start, end, flags);
        }
        int style = (Core.bool(run, "bold") ? Typeface.BOLD : 0)
                | (Core.bool(run, "italic") ? Typeface.ITALIC : 0);
        if (style != 0) {
            text.setSpan(new StyleSpan(style), start, end, flags);
        }
        if (Core.bool(run, "underline")) {
            text.setSpan(new UnderlineSpan(), start, end, flags);
        }
        if (!node.isEmpty()) {
            text.setSpan(new ClickableSpan() {
                @Override
                public void onClick(View widget) {
                    // A text field opens the editor; a checkbox, a radio and
                    // a link are the library's to act on (1650).
                    Map<String, Object> field = fields.get(node);
                    if (field != null && "text".equals(Core.str(field, "kind"))) {
                        fill(node, field);
                    } else {
                        verb("activate", node, "");
                    }
                }

                @Override
                public void updateDrawState(TextPaint paint) {
                    // The run's own inks and underline, not the link default.
                }
            }, start, end, flags);
        }
    }

    /**
     * A text field: the platform's editor, with what it holds and hiding it
     * when the page masks it (1650), its text handed in whole. Titled
     * "Input: <label>" with the page's own words before the field as its
     * hint, and the keyboard up as it shows (1667, walk 11, finding 59: a
     * title "q" and a keyboard that had to be asked for).
     */
    private void fill(final String node, Map<String, Object> shown) {
        verb("activate", node, "");
        final EditText field = new EditText(screen);
        field.setSingleLine(true);
        field.setTag("browse_field_editor");
        field.setInputType(Core.bool(shown, "masked")
                ? InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD
                : InputType.TYPE_CLASS_TEXT);
        field.setText(Core.str(shown, "value"));
        field.setSelection(field.getText().length());
        AlertDialog.Builder builder = new AlertDialog.Builder(screen)
                .setTitle(screen.getString(R.string.browse_field_title, Core.str(shown, "label")));
        String hint = FormField.hint(drawn, node);
        if (!hint.isEmpty()) {
            // The message stays above a prefilled field, where a hint would hide.
            builder.setMessage(hint);
        }
        AlertDialog dialog = builder
                .setView(field)
                .setPositiveButton(android.R.string.ok, new android.content.DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(android.content.DialogInterface dialog, int which) {
                        verb("fill", node, field.getText().toString());
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .create();
        android.view.Window window = dialog.getWindow();
        if (window != null) {
            window.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE);
        }
        dialog.show();
        field.requestFocus();
    }

    private int dp(int value) {
        return (int) (value * screen.getResources().getDisplayMetrics().density + 0.5f);
    }

    /** `#rrggbb` as a colour; a 256-colour index or nothing as none. */
    private static Integer ink(String ink) {
        if (ink.length() == 7 && ink.charAt(0) == '#') {
            try {
                return Color.parseColor(ink);
            } catch (IllegalArgumentException e) {
                return null;
            }
        }
        return null;
    }
}
