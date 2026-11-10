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
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

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
    private final LinearLayout rows;
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
        column.addView(bar);

        ScrollView scroll = new ScrollView(activity);
        LinearLayout body = new LinearLayout(activity);
        body.setOrientation(LinearLayout.VERTICAL);
        rows = new LinearLayout(activity);
        rows.setOrientation(LinearLayout.VERTICAL);
        body.addView(rows);
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
        body.addView(page);
        scroll.addView(body);
        FrameLayout stage = new FrameLayout(activity);
        stage.addView(scroll, new FrameLayout.LayoutParams(
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
        foot.addView(scrollButton("▲", "pageup", R.string.browse_page_up));
        foot.addView(scrollButton("▼", "pagedown", R.string.browse_page_down));
        column.addView(foot);

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
                verb("scroll", "", how);
            }
        });
        return button;
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
        int width = page.getWidth() - page.getPaddingLeft() - page.getPaddingRight();
        View scroll = (View) page.getParent().getParent();
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
        address.setHint(Core.str(shown, "address_hint"));
        String loading = Core.str(shown, "loading_words");
        loadingWords.setText(loading);
        loadingState.setVisibility(loading.isEmpty() ? View.GONE : View.VISIBLE);
        String said = Core.str(shown, "status");
        status.setText(said);
        // The status line says what the loading state does not: a cached
        // copy standing while the fresh one comes, a download.
        status.setVisibility(said.isEmpty() || !loading.isEmpty() ? View.INVISIBLE : View.VISIBLE);
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
            title.setPadding(16, 16, 16, 8);
            rows.addView(title);
        }
        for (Map<String, Object> row : Core.list(shown, "rows")) {
            rows.addView(row(row));
        }

        SpannableStringBuilder text = new SpannableStringBuilder();
        int widest = 0;
        for (Map<String, Object> line : Core.list(shown, "lines")) {
            int start = text.length();
            for (Map<String, Object> run : Core.list(line, "runs")) {
                append(text, run);
            }
            widest = Math.max(widest, text.length() - start);
            text.append('\n');
        }
        page.setText(text);
        // A page laid out wider than the view holds was laid out on cells
        // the library never heard of this view (a size said before the
        // session ran): say them again.
        if (cols > 0 && widest > cols && resentAt != cols) {
            resentAt = cols;
            cols = 0;
            size();
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
        view.setPadding(16, 12, 16, 12);
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
        boolean reverse = Core.bool(run, "reverse");
        Integer fg = ink(Core.str(run, reverse ? "bg" : "fg"));
        Integer bg = ink(Core.str(run, reverse ? "fg" : "bg"));
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
        final String node = Core.str(run, "node");
        if (!node.isEmpty()) {
            text.setSpan(new ClickableSpan() {
                @Override
                public void onClick(View widget) {
                    if (node.startsWith("field_")) {
                        fill(node);
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

    /** A form field: the platform's editor, its text handed in whole. */
    private void fill(final String node) {
        verb("activate", node, "");
        final EditText field = new EditText(screen);
        field.setSingleLine(true);
        new AlertDialog.Builder(screen)
                .setView(field)
                .setPositiveButton(android.R.string.ok, new android.content.DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(android.content.DialogInterface dialog, int which) {
                        verb("fill", node, field.getText().toString());
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
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
