package network.leviculum.loquax;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.os.Bundle;
import android.provider.Settings;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.text.format.DateUtils;
import android.text.format.Formatter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.Switch;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.RadioButton;
import android.widget.RadioGroup;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * The settings (1199), after the modern messenger cut: one list, each
 * entry a page of its own, this screen again with PAGE. The list is the
 * library's (1450): Profile, Mailbox, Appearance, Chats, Notifications,
 * Privacy, Maps, Backup, Advanced, the same sections in the same order on
 * every target; a page that has no controls yet says what exists today.
 * Profile opens the screen behind the avatar (ProfileActivity, 1187).
 *
 * Appearance holds the map's theme, which the map screen's moon held
 * before: like the phone, light or dark, the config's `map_theme` (1133).
 * Privacy holds the blocked contacts (1146), each with its avatar, name
 * and an "Unblock" that brings its conversation back on every device of
 * the home. Backup is the mailbox's backup when the mailbox is on this
 * phone (1152). Advanced names the relay (the propagation node), the only place the
 * app does, and ends the service.
 *
 * Chats holds the photo size (1220): small, phone (the default) or the
 * original, with the one line that says the photo's place and camera data
 * never go.
 *
 * Privacy starts with "Send read receipts" (1233), the core's switch:
 * off, no receipt goes out and none is shown.
 *
 * Maps (1201) is the management "Load area" was an action for: the
 * switch for the automatic download, the storage the regions take, and
 * each region with its size and date and a delete, and at the list's
 * foot one row that deletes them all (1290). Privacy lists the
 * positions the user shared, to whom and when, under the blocked
 * contacts.
 */
public class SettingsActivity extends Activity implements Events.Listener {
    /** The extra naming the page by its section's key; absent for the list. */
    static final String PAGE = "page";
    /** The extra carrying the page's title, as the list said it. */
    static final String PAGE_TITLE = "page_title";

    private LinearLayout body;
    /** The page's scroll view: the mailbox page keeps its position (1647). */
    private ScrollView scroll;
    /** The mailbox page's lines on screen, written in place on each ask (1647). */
    private final MailboxLines mailboxLines = new MailboxLines();
    /**
     * The page shown, its section's key (1450): the list's entries are the
     * library's one list of every target ({@link Core#settingsList}); null
     * for the list.
     */
    private String page;
    /** Between onResume and onPause: the mailbox page asks again only then. */
    private boolean resumed;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        page = getIntent().getStringExtra(PAGE);
        LinearLayout screen = new LinearLayout(this);
        screen.setOrientation(LinearLayout.VERTICAL);
        screen.setFitsSystemWindows(true);
        screen.setBackgroundColor(getColor(R.color.background));
        LinearLayout bar = new LinearLayout(this);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(4), 0, dp(4), 0);
        ImageButton back = new ImageButton(this);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, getColor(R.color.text)));
        back.setBackgroundResource(android.R.color.transparent);
        back.setContentDescription(getString(R.string.back));
        back.setPadding(dp(12), dp(12), dp(12), dp(12));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        bar.addView(back, new LinearLayout.LayoutParams(dp(48), dp(48)));
        String named = getIntent().getStringExtra(PAGE_TITLE);
        TextView title = text(page == null || named == null ? getString(R.string.settings) : named,
                20, R.color.text);
        title.setPadding(dp(12), 0, 0, 0);
        bar.addView(title);
        screen.addView(bar, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(64)));
        scroll = new ScrollView(this);
        body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        scroll.addView(body);
        screen.addView(scroll, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));
        setContentView(screen);
    }

    @Override
    protected void onStart() {
        super.onStart();
        link.bind();
    }

    @Override
    protected void onStop() {
        link.unbind();
        super.onStop();
    }

    @Override
    protected void onResume() {
        super.onResume();
        resumed = true;
        Events.listen(this);
        refresh();
    }

    @Override
    protected void onPause() {
        resumed = false;
        again.removeCallbacksAndMessages(null);
        Events.listen(null);
        super.onPause();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            refresh();
        } else if ("stopped".equals(kind)) {
            finish();
        }
    }

    private void refresh() {
        if ("mailbox".equals(page) && body.getChildCount() > 0) {
            // Asked again: written into the lines on screen, never an
            // emptied page (1647, walk 6, finding 28).
            mailbox();
            return;
        }
        body.removeAllViews();
        mailboxLines.clear();
        if ("mailbox".equals(page)) {
            mailbox();
        } else if ("appearance".equals(page)) {
            appearance();
        } else if ("chats".equals(page)) {
            chats();
        } else if ("notifications".equals(page)) {
            notifications();
        } else if ("privacy".equals(page)) {
            privacy();
        } else if ("maps".equals(page)) {
            maps(null, null);
        } else if ("backup".equals(page)) {
            backup();
        } else if ("advanced".equals(page)) {
            advanced();
        } else {
            entries();
        }
    }

    /**
     * The list: each section's title and what it holds, in the order of
     * the library's one list (1450), the same nine as the window's gear
     * and the terminal's `S`.
     */
    private void entries() {
        section(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.settingsList();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                for (Map<String, Object> section : Core.list(answer, "sections")) {
                    entry(Core.str(section, "key"), Core.str(section, "title"),
                            Core.str(section, "summary"));
                }
            }
        });
    }

    /** One section of the list; a tap opens its page. */
    private void entry(final String target, final String named, String said) {
        LinearLayout entry = new LinearLayout(this);
        entry.setOrientation(LinearLayout.VERTICAL);
        entry.setPadding(dp(16), dp(12), dp(16), dp(12));
        entry.setBackgroundResource(android.R.drawable.list_selector_background);
        entry.setId(R.id.settings_entry);
        entry.setTag("settings_" + target);
        TextView title = text(named, 16, R.color.text);
        title.setId(R.id.settings_entry_title);
        entry.addView(title);
        TextView summary = text(said, 14, R.color.text_secondary);
        summary.setId(R.id.settings_entry_summary);
        entry.addView(summary);
        entry.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // Profile is the screen behind the avatar (1187), not a page of its own.
                startActivity("profile".equals(target)
                        ? new Intent(SettingsActivity.this, ProfileActivity.class)
                        : new Intent(SettingsActivity.this, SettingsActivity.class)
                                .putExtra(PAGE, target).putExtra(PAGE_TITLE, named));
            }
        });
        body.addView(entry, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /**
     * A section drawn from the library's answer (1464): asked on the
     * engine's thread, drawn into a place of its own on the page, so the
     * page's order holds whichever answer comes first. The section adds to
     * {@link #body} as the others do: `body` is its place while it draws.
     */
    private void section(Engine.Ask<Map<String, Object>> ask,
            final Engine.Got<Map<String, Object>> draw) {
        final LinearLayout place = new LinearLayout(this);
        place.setOrientation(LinearLayout.VERTICAL);
        body.addView(place, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        Engine.ask(ask, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (isDestroyed()) {
                    return;
                }
                LinearLayout page = body;
                body = place;
                try {
                    draw.got(answer);
                } finally {
                    body = page;
                }
            }
        });
    }

    /**
     * The mailbox page (1642): the core's scene (`loquax::ui::mailbox_page`),
     * which mailbox, how the phone reaches it, which devices use it, drawn
     * group by group. While it is up it is asked again every few seconds, so
     * "since", the last contact and the mailbox's own answer move on it.
     *
     * <p>It holds still while it moves (1647, walk 6, finding 28: it jumped
     * to the top every five seconds): an answer of the page's shape is
     * written into the lines on screen ({@link MailboxLines}); one of
     * another shape is drawn whole when it is there, never into an emptied
     * page, and the scroll position is put back.
     */
    private void mailbox() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.mailboxPage();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (isDestroyed() || !"mailbox".equals(page)) {
                    return;
                }
                if (mailboxLines.fits(answer)) {
                    mailboxLines.said(answer);
                    return;
                }
                final int y = scroll.getScrollY();
                body.removeAllViews();
                List<MailboxLines.Slot> slots = new ArrayList<MailboxLines.Slot>();
                mailbox(answer, slots);
                mailboxLines.drawn(answer, slots);
                scroll.post(new Runnable() {
                    @Override
                    public void run() {
                        scroll.scrollTo(0, y);
                    }
                });
            }
        });
        again.removeCallbacks(mailboxAgain);
        again.postDelayed(mailboxAgain, MAILBOX_AGAIN_MS);
    }

    /** How often the mailbox page asks again while it is up. */
    private static final long MAILBOX_AGAIN_MS = 5000;
    private final android.os.Handler again = new android.os.Handler(android.os.Looper.getMainLooper());
    private final Runnable mailboxAgain = new Runnable() {
        @Override
        public void run() {
            if (!isDestroyed() && resumed && "mailbox".equals(page)) {
                refresh();
            }
        }
    };

    private void mailbox(Map<String, Object> answer, List<MailboxLines.Slot> slots) {
        String error = Core.str(answer, "error");
        if (!error.isEmpty()) {
            body.addView(padded(text(error, 15, R.color.text_secondary)));
            return;
        }
        final String copyLabel = Core.str(answer, "copy_label");
        final String copied = Core.str(answer, "copied");
        for (Map<String, Object> group : Core.list(answer, "groups")) {
            TextView header = text(Core.str(group, "title"), 14, R.color.accent);
            header.setPadding(dp(16), dp(16), dp(16), dp(4));
            body.addView(header);
            for (Map<String, Object> line : Core.list(group, "lines")) {
                slots.add(mailboxLine(line, copyLabel, copied));
            }
        }
    }

    /**
     * One line of the mailbox page: label, value under it, a copy button, a
     * tap. Its slot writes a later answer's words into the same views.
     */
    private MailboxLines.Slot mailboxLine(Map<String, Object> line, String copyLabel,
            final String copied) {
        String label = Core.str(line, "label");
        if (Core.bool(line, "note")) {
            final TextView note = text(label, 13, R.color.text_secondary);
            note.setPadding(dp(16), 0, dp(16), dp(8));
            body.addView(note);
            return new MailboxLines.Slot() {
                @Override
                public void said(String label, String value) {
                    same(note, label);
                }
            };
        }
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setMinimumHeight(dp(48));
        row.setPadding(dp(16), dp(6), dp(8), dp(6));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        final String action = Core.str(line, "action");
        final TextView labelView = text(label, 16,
                "profile".equals(action) ? R.color.accent : R.color.text);
        words.addView(labelView);
        String value = Core.str(line, "value");
        TextView valueView = null;
        if (!value.isEmpty()) {
            valueView = text(value, 14, R.color.text_secondary);
            valueView.setTextIsSelectable(true);
            words.addView(valueView);
        }
        final TextView said = valueView;
        row.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        final String copy = Core.str(line, "copy");
        if (!copy.isEmpty()) {
            Button button = new Button(this);
            button.setText(copyLabel);
            button.setAllCaps(false);
            button.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    android.content.ClipboardManager clipboard = (android.content.ClipboardManager)
                            getSystemService(CLIPBOARD_SERVICE);
                    if (clipboard != null) {
                        clipboard.setPrimaryClip(android.content.ClipData.newPlainText(copied, copy));
                    }
                    // Android 13 shows its own preview of what was copied;
                    // a toast on top says it twice (1647, walk 6, finding 35).
                    if (android.os.Build.VERSION.SDK_INT < 33) {
                        android.widget.Toast.makeText(SettingsActivity.this, copied,
                                android.widget.Toast.LENGTH_SHORT).show();
                    }
                }
            });
            row.addView(button);
        }
        if ("profile".equals(action)) {
            row.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startActivity(new Intent(SettingsActivity.this, ProfileActivity.class));
                }
            });
        }
        body.addView(row, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        return new MailboxLines.Slot() {
            @Override
            public void said(String label, String value) {
                same(labelView, label);
                if (said != null) {
                    same(said, value);
                }
            }
        };
    }

    /**
     * Write `words` into `view` only where they differ: a selectable value
     * keeps its selection, and an unchanged line is not laid out again.
     */
    private static void same(TextView view, String words) {
        if (!words.contentEquals(view.getText())) {
            view.setText(words);
        }
    }

    /**
     * The map's theme, a choice of three (1199): like the phone, light,
     * dark. The renderer stores it as the config's `map_theme` and draws
     * the next frame in it.
     */
    private void appearance() {
        TextView header = text(getString(R.string.settings_map_theme), 14, R.color.accent);
        header.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(header);
        final String[] words = {"auto", "light", "dark"};
        int[] labels = {R.string.map_theme_auto, R.string.map_theme_light, R.string.map_theme_dark};
        final RadioButton[] choices = new RadioButton[words.length];
        RadioGroup group = new RadioGroup(this);
        group.setPadding(dp(16), 0, dp(16), dp(8));
        for (int i = 0; i < words.length; i++) {
            final String word = words[i];
            RadioButton choice = new RadioButton(this);
            choice.setId(View.generateViewId());
            choice.setText(labels[i]);
            choice.setTextSize(16);
            choice.setTextColor(getColor(R.color.text));
            choice.setMinHeight(dp(48));
            choice.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    theme(word, choices, words);
                }
            });
            choices[i] = choice;
            group.addView(choice);
        }
        body.addView(group);
        body.addView(padded(text(getString(R.string.settings_app_theme_text), 14, R.color.text_secondary)));
        theme(null, choices, words);
    }

    /**
     * Store `set` as the map's theme, or with null only ask, off the UI
     * thread; the choice in force checked after, a failure said.
     */
    private void theme(final String set, final RadioButton[] choices,
            final String[] words) {
        new Thread(new Runnable() {
            @Override
            public void run() {
                final Map<String, Object> answer = Core.mapTheme(set);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (!Core.bool(answer, "ok")) {
                            android.widget.Toast.makeText(SettingsActivity.this,
                                    Core.str(answer, "error"), android.widget.Toast.LENGTH_LONG).show();
                            if (set != null) {
                                theme(null, choices, words);
                            }
                            return;
                        }
                        String chosen = Core.str(answer, "theme");
                        for (int i = 0; i < words.length; i++) {
                            choices[i].setChecked(words[i].equals(chosen));
                        }
                    }
                });
            }
        }, "loquax-theme").start();
    }

    /**
     * The photo size (1220), a choice of three, phone the default: the
     * library keeps it as the config's `photo_size` and scales by it.
     */
    private void chats() {
        TextView header = text(getString(R.string.photo_size), 14, R.color.accent);
        header.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(header);
        final String[] words = {"small", "phone", "original"};
        int[] labels = {R.string.photo_small, R.string.photo_phone, R.string.photo_original};
        int[] ids = {R.id.photo_size_small, R.id.photo_size_phone, R.id.photo_size_original};
        final RadioButton[] choices = new RadioButton[words.length];
        RadioGroup group = new RadioGroup(this);
        group.setPadding(dp(16), 0, dp(16), dp(8));
        for (int i = 0; i < words.length; i++) {
            final String word = words[i];
            RadioButton choice = new RadioButton(this);
            choice.setId(ids[i]);
            choice.setText(labels[i]);
            choice.setTextSize(16);
            choice.setTextColor(getColor(R.color.text));
            choice.setMinHeight(dp(48));
            choice.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    photoSize(word, choices, words);
                }
            });
            choices[i] = choice;
            group.addView(choice);
        }
        body.addView(group);
        body.addView(padded(text(getString(R.string.photo_size_text), 14, R.color.text_secondary)));
        photoSize(null, choices, words);
    }

    /** Store `set` as the photo size, or with null only ask; as theme(). */
    private void photoSize(final String set, final RadioButton[] choices, final String[] words) {
        // Asked on the engine's thread (1464).
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.photoSize(set);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (!isDestroyed()) {
                    photoSize(answer, set, choices, words);
                }
            }
        });
    }

    private void photoSize(Map<String, Object> answer, String set, RadioButton[] choices,
            String[] words) {
        if (!Core.bool(answer, "ok")) {
            android.widget.Toast.makeText(SettingsActivity.this,
                    Core.str(answer, "error"), android.widget.Toast.LENGTH_LONG).show();
            if (set != null) {
                photoSize(null, choices, words);
            }
            return;
        }
        String chosen = Core.str(answer, "size");
        for (int i = 0; i < words.length; i++) {
            choices[i].setChecked(words[i].equals(chosen));
        }
    }

    /** The read receipts switch, the blocked contacts, the positions shared. */
    private void privacy() {
        receipts();
        TextView header = text(getString(R.string.blocked_contacts), 14, R.color.accent);
        header.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(header);
        blockedRows();
        runningShares();
        sharedPositions();
    }

    /** The blocked contacts' rows, as the library lists them. */
    private void blockedRows() {
        section(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.blocked();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                blockedRows(answer);
            }
        });
    }

    private void blockedRows(Map<String, Object> answer) {
        List<Map<String, Object>> rows = Core.list(answer, "conversations");
        if (answer.containsKey("error")) {
            body.addView(padded(text(Core.str(answer, "error"), 15, R.color.text_secondary)));
        } else if (rows.isEmpty()) {
            body.addView(padded(text(getString(R.string.blocked_none), 15, R.color.text_secondary)));
        }
        for (Map<String, Object> row : rows) {
            body.addView(blocked(row));
        }
    }

    /**
     * "Position sharing" (1273): every running share by the library's
     * list, with whom and for how long, each with its Stop.
     */
    private void runningShares() {
        section(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.shares();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                runningShares(answer);
            }
        });
    }

    private void runningShares(Map<String, Object> answer) {
        TextView header = text(Core.str(answer, "title"), 14, R.color.accent);
        header.setPadding(dp(16), dp(24), dp(16), dp(4));
        body.addView(header);
        if (answer.containsKey("error")) {
            body.addView(padded(text(Core.str(answer, "error"), 15, R.color.text_secondary)));
            return;
        }
        List<Map<String, Object>> rows = Core.list(answer, "shares");
        if (rows.isEmpty()) {
            body.addView(padded(text(Core.str(answer, "empty"), 15, R.color.text_secondary)));
        }
        for (Map<String, Object> row : rows) {
            final String peer = Core.str(row, "peer");
            LinearLayout line = new LinearLayout(this);
            line.setTag(Core.str(row, "id"));
            line.setGravity(Gravity.CENTER_VERTICAL);
            line.setPadding(dp(16), dp(8), dp(4), dp(8));
            LinearLayout words = new LinearLayout(this);
            words.setOrientation(LinearLayout.VERTICAL);
            words.addView(text(Core.str(row, "name"), 16, R.color.text));
            words.addView(text(Core.str(row, "words"), 14, R.color.text_secondary));
            line.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
            Button stop = new Button(this, null, android.R.attr.borderlessButtonStyle);
            stop.setText(Core.str(answer, "stop"));
            stop.setTextColor(getColor(R.color.accent));
            stop.setTag("share_stop_" + peer);
            stop.setContentDescription(Core.str(answer, "stop") + ", " + Core.str(row, "name"));
            stop.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    Engine.run(new Engine.Call() {
                        @Override
                        public String run() {
                            return Core.stopShare(peer);
                        }
                    }, new Engine.Said() {
                        @Override
                        public void said(String error) {
                            if (error != null) {
                                android.widget.Toast.makeText(SettingsActivity.this, error,
                                        android.widget.Toast.LENGTH_LONG).show();
                            }
                            ShareService.look(SettingsActivity.this, false);
                            recreate();
                        }
                    });
                }
            });
            line.addView(stop);
            body.addView(line);
        }
    }

    /**
     * The Maps page (1201): ask the renderer off the UI thread, with a
     * region to delete or the autoload to switch when given, and show
     * what it answers; a change refused is said and the page asked again,
     * so the switch shows what is stored (1207).
     */
    private void maps(final String delete, final Boolean autoload) {
        new Thread(new Runnable() {
            @Override
            public void run() {
                final Map<String, Object> answer = Core.mapRegions(delete, autoload);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (!"maps".equals(page) || isFinishing()) {
                            return;
                        }
                        if (!Core.bool(answer, "ok") && (delete != null || autoload != null)) {
                            android.widget.Toast.makeText(SettingsActivity.this,
                                    Core.str(answer, "error"), android.widget.Toast.LENGTH_LONG).show();
                            maps(null, null);
                            return;
                        }
                        showMaps(answer);
                    }
                });
            }
        }, "loquax-regions").start();
    }

    /** The switch, the storage used and the regions, as the renderer listed them. */
    private void showMaps(Map<String, Object> answer) {
        body.removeAllViews();
        if (!Core.bool(answer, "ok")) {
            android.widget.Toast.makeText(this, Core.str(answer, "error"),
                    android.widget.Toast.LENGTH_LONG).show();
        }
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(12), dp(16), dp(12));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        words.addView(text(getString(R.string.maps_auto), 16, R.color.text));
        words.addView(text(getString(R.string.maps_auto_text), 14, R.color.text_secondary));
        row.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        final Switch auto = new Switch(this);
        auto.setId(R.id.maps_auto);
        auto.setChecked(Core.bool(answer, "autoload"));
        auto.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                maps(null, auto.isChecked());
            }
        });
        row.addView(auto);
        body.addView(row, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        TextView used = text(getString(R.string.maps_used,
                Formatter.formatShortFileSize(this, Core.num(answer, "region_bytes"))), 14, R.color.accent);
        used.setId(R.id.maps_used);
        used.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(used);
        List<Map<String, Object>> regions = Core.list(answer, "regions");
        if (regions.isEmpty()) {
            body.addView(padded(text(getString(R.string.maps_none), 15, R.color.text_secondary)));
        }
        for (Map<String, Object> region : regions) {
            body.addView(region(region));
        }
        body.addView(deleteAll(regions.size(), Core.num(answer, "region_bytes"),
                Core.bool(answer, "autoload")));
    }

    /**
     * The list's last row (1290): "Delete all downloaded maps (1.2 GB)",
     * off without maps; a tap asks once, in words that say whether the
     * automatic download brings them back, then deletes and shows the
     * page again.
     */
    private View deleteAll(final int count, long bytes, final boolean autoload) {
        Button all = new Button(this);
        all.setId(R.id.maps_delete_all);
        all.setText(getString(R.string.maps_delete_all, Formatter.formatShortFileSize(this, bytes)));
        all.setEnabled(count > 0);
        all.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                int ask = autoload ? R.string.maps_delete_all_ask : R.string.maps_delete_all_ask_manual;
                new AlertDialog.Builder(SettingsActivity.this)
                        .setMessage(getString(ask, count))
                        .setPositiveButton(R.string.maps_delete_all_yes, new DialogInterface.OnClickListener() {
                            @Override
                            public void onClick(DialogInterface dialog, int which) {
                                deleteAllRegions();
                            }
                        })
                        .setNegativeButton(android.R.string.cancel, null)
                        .show();
            }
        });
        LinearLayout row = new LinearLayout(this);
        row.setPadding(dp(16), dp(16), dp(16), dp(16));
        row.addView(all, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        return row;
    }

    /** Delete every region off the UI thread, say a refusal, then ask the page again. */
    private void deleteAllRegions() {
        new Thread(new Runnable() {
            @Override
            public void run() {
                final String error = Core.deleteRegions();
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (error != null && !isFinishing()) {
                            android.widget.Toast.makeText(SettingsActivity.this, error,
                                    android.widget.Toast.LENGTH_LONG).show();
                        }
                        maps(null, null);
                    }
                });
            }
        }, "loquax-regions-all").start();
    }

    /** One region: where it is, its size, date and zooms, and Delete with a question first. */
    private View region(Map<String, Object> region) {
        final String name = Core.str(region, "name");
        LinearLayout line = new LinearLayout(this);
        line.setId(R.id.maps_region);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(16), dp(8), dp(8), dp(8));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        TextView place = text(getString(R.string.maps_region,
                Ui.place(Core.num(region, "lat_e6"), Core.num(region, "lon_e6"))), 16, R.color.text);
        place.setId(R.id.maps_region_name);
        words.addView(place);
        long modified = Core.num(region, "modified");
        String when = modified == 0 ? "" : DateUtils.formatDateTime(this, modified * 1000,
                DateUtils.FORMAT_SHOW_DATE | DateUtils.FORMAT_ABBREV_MONTH);
        TextView facts = text(getString(R.string.maps_region_line,
                Formatter.formatShortFileSize(this, Core.num(region, "bytes")), when,
                (int) Core.num(region, "min_zoom"), (int) Core.num(region, "max_zoom")), 14,
                R.color.text_secondary);
        facts.setId(R.id.maps_region_line);
        words.addView(facts);
        line.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button delete = new Button(this);
        delete.setText(R.string.maps_delete);
        delete.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                new AlertDialog.Builder(SettingsActivity.this)
                        .setMessage(R.string.maps_delete_ask)
                        .setPositiveButton(R.string.maps_delete, new DialogInterface.OnClickListener() {
                            @Override
                            public void onClick(DialogInterface dialog, int which) {
                                maps(name, null);
                            }
                        })
                        .setNegativeButton(android.R.string.cancel, null)
                        .show();
            }
        });
        line.addView(delete);
        return line;
    }

    /**
     * "Send read receipts" (1233), first on the page: the switch as the
     * core holds it; a change goes to the core, which keeps it in the
     * config, and the switch shows what the core answered.
     */
    private void receipts() {
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(12), dp(16), dp(12));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        words.addView(text(getString(R.string.read_receipts), 16, R.color.text));
        words.addView(text(getString(R.string.read_receipts_text), 14, R.color.text_secondary));
        row.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        final Switch on = new Switch(this);
        on.setId(R.id.privacy_receipts);
        on.setContentDescription(getString(R.string.read_receipts));
        // Asked and set on the engine's thread (1464).
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.receipts(null);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                on.setChecked(Core.bool(answer, "on"));
            }
        });
        on.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                final boolean wanted = on.isChecked();
                Engine.ask(new Engine.Ask<Map<String, Object>>() {
                    @Override
                    public Map<String, Object> ask() {
                        return Core.receipts(wanted);
                    }
                }, new Engine.Got<Map<String, Object>>() {
                    @Override
                    public void got(Map<String, Object> answer) {
                        if (isDestroyed()) {
                            return;
                        }
                        if (answer.containsKey("error")) {
                            android.widget.Toast.makeText(SettingsActivity.this,
                                    Core.str(answer, "error"), android.widget.Toast.LENGTH_LONG).show();
                            on.setChecked(!wanted);
                            return;
                        }
                        on.setChecked(Core.bool(answer, "on"));
                    }
                });
            }
        });
        row.addView(on);
        body.addView(row, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** The positions the user shared (1201): to whom, and when. */
    private void sharedPositions() {
        TextView header = text(getString(R.string.privacy_shared), 14, R.color.accent);
        header.setPadding(dp(16), dp(24), dp(16), dp(4));
        body.addView(header);
        sharedRows();
    }

    /** The shared positions' rows, as the library lists them. */
    private void sharedRows() {
        section(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.shared();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                sharedRows(answer);
            }
        });
    }

    private void sharedRows(Map<String, Object> answer) {
        List<Map<String, Object>> rows = Core.list(answer, "shared");
        if (answer.containsKey("error")) {
            body.addView(padded(text(Core.str(answer, "error"), 15, R.color.text_secondary)));
            return;
        }
        if (rows.isEmpty()) {
            body.addView(padded(text(getString(R.string.privacy_shared_none), 15, R.color.text_secondary)));
        }
        for (Map<String, Object> row : rows) {
            String peer = Core.str(row, "peer");
            LinearLayout line = new LinearLayout(this);
            line.setId(R.id.privacy_shared_row);
            line.setGravity(Gravity.CENTER_VERTICAL);
            line.setPadding(dp(16), dp(8), dp(16), dp(8));
            AvatarView avatar = new AvatarView(this, null);
            avatar.show(peer, row);
            line.addView(avatar, new LinearLayout.LayoutParams(dp(40), dp(40)));
            TextView name = text(Core.str(row, "name"), 16, R.color.text);
            name.setPadding(dp(16), 0, dp(8), 0);
            name.setSingleLine(true);
            name.setEllipsize(android.text.TextUtils.TruncateAt.END);
            line.addView(name, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
            line.addView(text(DateUtils.getRelativeTimeSpanString(Core.num(row, "at") * 1000,
                    System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS).toString(), 14,
                    R.color.text_secondary));
            body.addView(line);
        }
    }

    /**
     * What Loquax notifies of, and the connection notice's row (1345): it
     * opens the session channel's system page, where the user picks
     * minimised or off, as ntfy and Conversations do; Android keeps a
     * foreground service's notice, and only the user may lower its channel
     * (LoquaxService.channels).
     */
    private void notifications() {
        body.addView(padded(text(getString(R.string.settings_notifications_text), 15,
                R.color.text_secondary)));
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.VERTICAL);
        row.setPadding(dp(16), dp(12), dp(16), dp(12));
        row.setBackgroundResource(android.R.drawable.list_selector_background);
        row.setId(R.id.connection_notice);
        row.addView(text(getString(R.string.connection_notice), 16, R.color.text));
        row.addView(text(getString(R.string.connection_notice_text), 14, R.color.text_secondary));
        row.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // The channel has to exist for its page; the service makes
                // it too, but may not have run yet.
                LoquaxService.channels(SettingsActivity.this);
                try {
                    startActivity(new Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
                            .putExtra(Settings.EXTRA_APP_PACKAGE, getPackageName())
                            .putExtra(Settings.EXTRA_CHANNEL_ID, LoquaxService.SESSION_CHANNEL));
                } catch (ActivityNotFoundException none) {
                    // A system without the channel page: the app's page lists the channels.
                    startActivity(new Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                            .putExtra(Settings.EXTRA_APP_PACKAGE, getPackageName()));
                }
            }
        });
        body.addView(row, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** The mailbox's backup when it is on this phone. */
    private void backup() {
        section(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.me();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                backup(answer);
            }
        });
    }

    private void backup(Map<String, Object> answer) {
        boolean phone = Core.bool(answer, "here");
        if (!phone) {
            body.addView(padded(text(getString(R.string.settings_backup_away), 15,
                    R.color.text_secondary)));
            return;
        }
        TextView backup = text(getString(R.string.backup_title), 16, R.color.text);
        backup.setPadding(dp(16), dp(12), dp(16), dp(12));
        backup.setBackgroundResource(android.R.drawable.list_selector_background);
        backup.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(SettingsActivity.this, BackupActivity.class));
            }
        });
        body.addView(backup, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** The relay (the propagation node), the map's fling, and the service's end. */
    private void advanced() {
        mapFling(null);
        TextView node = text(getString(R.string.propagation_node), 16, R.color.text);
        node.setPadding(dp(16), dp(16), dp(16), 0);
        body.addView(node);
        body.addView(padded(text(getString(R.string.propagation_node_text), 14,
                R.color.text_secondary)));
        hubs = new LinearLayout(this);
        hubs.setOrientation(LinearLayout.VERTICAL);
        body.addView(hubs, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        roomsHub(null);
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(16), dp(16), dp(4));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        words.addView(text(getString(R.string.map_fling), 16, R.color.text));
        words.addView(text(getString(R.string.map_fling_text), 14, R.color.text_secondary));
        row.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        final Switch flings = new Switch(this);
        flings.setId(R.id.map_fling);
        flings.setChecked(true);
        flings.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                mapFling(flings.isChecked());
            }
        });
        row.addView(flings);
        body.addView(row, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        // The verb and, under it, what it costs (1447, walk 1 finding 27):
        // nothing arrives until the app is opened again.
        LinearLayout stop = new LinearLayout(this);
        stop.setOrientation(LinearLayout.VERTICAL);
        stop.addView(text(getString(R.string.stop_service), 16, R.color.text));
        stop.addView(text(getString(R.string.stop_service_text), 14, R.color.text_secondary));
        stop.setPadding(dp(16), dp(24), dp(16), dp(16));
        stop.setBackgroundResource(android.R.drawable.list_selector_background);
        stop.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // What the notification's stop button does: the service
                // ends the session, `stopped` closes the screens.
                startService(new Intent(SettingsActivity.this, LoquaxService.class)
                        .setAction(LoquaxService.STOP));
            }
        });
        body.addView(stop, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** The public rooms' hub lines (1577), filled by {@link #roomsHub}. */
    private LinearLayout hubs;

    /**
     * The public rooms' hub (1577): `set` chosen when given ("none", or a
     * hub's address), then the lines as the core answers them, the same
     * as the window's and the terminal's Advanced section: which hub, the
     * shipped ones with a mark on the one in force, none, a field to
     * paste another, and what a hub learns. Asked on the engine's thread.
     */
    private void roomsHub(final String set) {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.roomsHub(set);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (isDestroyed()) {
                    return;
                }
                String error = Core.str(answer, "error");
                if (!error.isEmpty()) {
                    android.widget.Toast.makeText(SettingsActivity.this, error,
                            android.widget.Toast.LENGTH_LONG).show();
                }
                hubLines(answer);
            }
        });
    }

    private void hubLines(Map<String, Object> answer) {
        hubs.removeAllViews();
        TextView title = text(Core.str(answer, "title"), 16, R.color.text);
        title.setPadding(dp(16), dp(24), dp(16), 0);
        hubs.addView(title);
        hubs.addView(padded(text(Core.str(answer, "current"), 14, R.color.text_secondary)));
        for (Map<String, Object> hub : Core.list(answer, "hubs")) {
            final String set = Core.str(hub, "set");
            String mark = Core.bool(hub, "chosen") ? "\u2713  " : "     ";
            // 1599: how far the hub is and its marks, under its name.
            String detail = Core.str(hub, "detail");
            TextView line = text(mark + Core.str(hub, "name")
                    + (detail.isEmpty() ? "" : "\n     " + detail), 16, R.color.text);
            line.setPadding(dp(16), dp(12), dp(16), dp(12));
            line.setTag("rooms_hub_" + set);
            line.setBackgroundResource(android.R.drawable.list_selector_background);
            line.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    roomsHub(set);
                }
            });
            hubs.addView(line, new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        }
        String own = Core.str(answer, "own");
        TextView paste = text((own.isEmpty() ? "     " : "\u2713  ") + Core.str(answer, "paste")
                + (own.isEmpty() ? "" : ": " + own), 16, R.color.text);
        paste.setPadding(dp(16), dp(12), dp(16), 0);
        hubs.addView(paste);
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), 0, dp(16), dp(4));
        final EditText field = new EditText(this);
        field.setHint(Core.str(answer, "hint"));
        field.setSingleLine(true);
        field.setTag("rooms_hub_field");
        row.addView(field, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button take = new Button(this);
        take.setText(android.R.string.ok);
        take.setTag("rooms_hub_take");
        take.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String typed = field.getText().toString().trim();
                if (!typed.isEmpty()) {
                    roomsHub(typed);
                }
            }
        });
        row.addView(take);
        hubs.addView(row, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        hubs.addView(padded(text(Core.str(answer, "note"), 14, R.color.text_secondary)));
    }

    /**
     * The map's fling (1188): `set` stored when given, then the switch
     * shows the choice in force; off the UI thread, as the renderer may
     * be drawing.
     */
    private void mapFling(final Boolean set) {
        new Thread(new Runnable() {
            @Override
            public void run() {
                final Map<String, Object> answer = Core.mapFling(set);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (!Core.bool(answer, "ok")) {
                            android.widget.Toast.makeText(SettingsActivity.this,
                                    Core.str(answer, "error"), android.widget.Toast.LENGTH_LONG).show();
                        }
                        View shown = body.findViewById(R.id.map_fling);
                        if (shown instanceof Switch) {
                            ((Switch) shown).setChecked(Core.bool(answer, "fling"));
                        }
                    }
                });
            }
        }, "loquax-fling").start();
    }

    /** One blocked peer: its avatar and name as every screen shows it, and "Unblock". */
    private View blocked(Map<String, Object> row) {
        final String peer = Core.str(row, "peer");
        LinearLayout line = new LinearLayout(this);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(16), dp(8), dp(8), dp(8));
        AvatarView avatar = new AvatarView(this, null);
        avatar.show(peer, row);
        line.addView(avatar, new LinearLayout.LayoutParams(dp(40), dp(40)));
        TextView name = text(Core.str(row, "name"), 16, R.color.text);
        name.setPadding(dp(16), 0, dp(8), 0);
        line.addView(name, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button unblock = new Button(this);
        unblock.setText(R.string.unblock);
        unblock.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Engine.run(new Engine.Call() {
                    @Override
                    public String run() {
                        return Core.block(peer, false);
                    }
                }, new Engine.Said() {
                    @Override
                    public void said(String error) {
                        refresh();
                    }
                });
            }
        });
        line.addView(unblock);
        return line;
    }

    private View padded(TextView view) {
        view.setPadding(dp(16), dp(8), dp(16), dp(8));
        return view;
    }

    private TextView text(String words, float sp, int colour) {
        TextView view = new TextView(this);
        view.setText(words);
        view.setTextSize(sp);
        view.setTextColor(getColor(colour));
        return view;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}
