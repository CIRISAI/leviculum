package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Typeface;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.TextPaint;
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
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
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
 */
final class BrowsePane {
    /** What the pane hands back to the Activity. */
    interface Host {
        /** The tab gave way to a chat (a page's link to its operator). */
        void left();
    }

    private final Activity screen;
    private final Host host;
    private final ImageButton back;
    private final EditText address;
    private final TextView page;
    private final LinearLayout rows;
    private final TextView status;
    private int cols;
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
        bar.addView(back);
        address = new EditText(activity);
        address.setSingleLine(true);
        address.setTypeface(Typeface.MONOSPACE);
        address.setImeOptions(EditorInfo.IME_ACTION_GO);
        address.setTag("address");
        address.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                verb("go", "", v.getText().toString());
                return true;
            }
        });
        bar.addView(address, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button places = new Button(activity);
        places.setText("☰");
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
        body.addView(page);
        scroll.addView(body);
        column.addView(scroll, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout foot = new LinearLayout(activity);
        foot.setGravity(Gravity.CENTER_VERTICAL);
        status = new TextView(activity);
        status.setTextColor(activity.getColor(R.color.text_secondary));
        foot.addView(status, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        foot.addView(scrollButton("▲", "pageup"));
        foot.addView(scrollButton("▼", "pagedown"));
        column.addView(foot);

        // The page is laid out on the cells the view holds.
        page.addOnLayoutChangeListener(new View.OnLayoutChangeListener() {
            @Override
            public void onLayoutChange(View v, int l, int t, int r, int b, int ol, int ot, int or, int ob) {
                size();
            }
        });
    }

    private Button scrollButton(String label, final String how) {
        Button button = new Button(screen);
        button.setText(label);
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
        address.setHint(Core.str(shown, "address_hint"));
        status.setText(Core.str(shown, "status"));
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
        for (Map<String, Object> line : Core.list(shown, "lines")) {
            for (Map<String, Object> run : Core.list(line, "runs")) {
                append(text, run);
            }
            text.append('\n');
        }
        page.setText(text);
    }

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
