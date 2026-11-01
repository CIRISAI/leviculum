package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.text.format.DateUtils;
import android.text.format.Formatter;
import android.widget.Button;
import android.widget.Switch;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.RadioButton;
import android.widget.RadioGroup;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.List;
import java.util.Map;

/**
 * The settings (1199), after the modern messenger cut: one list, each
 * entry a page of its own, this screen again with PAGE. Profile, Mailbox,
 * Appearance, Notifications, Privacy, Maps, Backup, Advanced; a page that
 * has no controls yet says what exists today. Profile opens the screen
 * behind the avatar (ProfileActivity, 1187).
 *
 * Appearance holds the map's theme, which the map screen's moon held
 * before: like the phone, light or dark, the config's `map_theme` (1133).
 * Privacy holds the blocked contacts (1146), each with its avatar, name
 * and an "Unblock" that brings its conversation back on every device of
 * the home. Backup is the mailbox's backup when the mailbox is on this
 * phone (1152). Advanced names the Propagation Node, the only place the
 * app does, and ends the service.
 *
 * Privacy starts with "Send read receipts" (1233), the core's switch:
 * off, no receipt goes out and none is shown.
 *
 * Maps (1201) is the management "Load area" was an action for: the
 * switch for the automatic download, the storage the regions take, and
 * each region with its size and date and a delete. Privacy lists the
 * positions the user shared, to whom and when, under the blocked
 * contacts.
 */
public class SettingsActivity extends Activity implements Events.Listener {
    /** The extra naming the page; absent for the list. */
    static final String PAGE = "page";

    /** The pages in the list's order, by their title's string. */
    private static final int[] PAGES = {
        R.string.settings_profile, R.string.mailbox, R.string.settings_appearance,
        R.string.settings_notifications, R.string.settings_privacy, R.string.settings_maps,
        R.string.settings_backup, R.string.advanced,
    };
    /** What each page holds, one line under its title in the list. */
    private static final int[] SUMMARIES = {
        R.string.settings_profile_summary, R.string.settings_mailbox_summary,
        R.string.settings_appearance_summary, R.string.settings_notifications_summary,
        R.string.settings_privacy_summary, R.string.settings_maps_summary,
        R.string.settings_backup_summary, R.string.settings_advanced_summary,
    };

    private LinearLayout body;
    /** The page shown, a string of PAGES; 0 for the list. */
    private int page;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        page = getIntent().getIntExtra(PAGE, 0);
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
        TextView title = text(getString(page == 0 ? R.string.settings : page), 20, R.color.text);
        title.setPadding(dp(12), 0, 0, 0);
        bar.addView(title);
        screen.addView(bar, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(64)));
        ScrollView scroll = new ScrollView(this);
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
        Events.listen(this);
        refresh();
    }

    @Override
    protected void onPause() {
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
        body.removeAllViews();
        if (page == R.string.mailbox) {
            mailbox();
        } else if (page == R.string.settings_appearance) {
            appearance();
        } else if (page == R.string.settings_notifications) {
            body.addView(padded(text(getString(R.string.settings_notifications_text), 15,
                    R.color.text_secondary)));
        } else if (page == R.string.settings_privacy) {
            privacy();
        } else if (page == R.string.settings_maps) {
            maps(null, null);
        } else if (page == R.string.settings_backup) {
            backup();
        } else if (page == R.string.advanced) {
            advanced();
        } else {
            entries();
        }
    }

    /** The list: each page's title and what it holds. */
    private void entries() {
        for (int i = 0; i < PAGES.length; i++) {
            final int target = PAGES[i];
            LinearLayout entry = new LinearLayout(this);
            entry.setOrientation(LinearLayout.VERTICAL);
            entry.setPadding(dp(16), dp(12), dp(16), dp(12));
            entry.setBackgroundResource(android.R.drawable.list_selector_background);
            entry.setId(R.id.settings_entry);
            TextView title = text(getString(target), 16, R.color.text);
            title.setId(R.id.settings_entry_title);
            entry.addView(title);
            TextView summary = text(getString(SUMMARIES[i]), 14, R.color.text_secondary);
            summary.setId(R.id.settings_entry_summary);
            entry.addView(summary);
            entry.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    // Profile is the screen behind the avatar (1187), not a page of its own.
                    startActivity(target == R.string.settings_profile
                            ? new Intent(SettingsActivity.this, ProfileActivity.class)
                            : new Intent(SettingsActivity.this, SettingsActivity.class)
                                    .putExtra(PAGE, target));
                }
            });
            body.addView(entry, new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        }
    }

    /** Where the mailbox is. */
    private void mailbox() {
        boolean phone = Core.bool(Core.me(), "here");
        body.addView(padded(text(getString(phone ? R.string.mailbox_here : R.string.mailbox_away),
                15, R.color.text_secondary)));
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

    /** The read receipts switch, the blocked contacts, the positions shared. */
    private void privacy() {
        receipts();
        TextView header = text(getString(R.string.blocked_contacts), 14, R.color.accent);
        header.setPadding(dp(16), dp(16), dp(16), dp(4));
        body.addView(header);
        Map<String, Object> answer = Core.blocked();
        List<Map<String, Object>> rows = Core.list(answer, "conversations");
        if (answer.containsKey("error")) {
            body.addView(padded(text(Core.str(answer, "error"), 15, R.color.text_secondary)));
        } else if (rows.isEmpty()) {
            body.addView(padded(text(getString(R.string.blocked_none), 15, R.color.text_secondary)));
        }
        for (Map<String, Object> row : rows) {
            body.addView(blocked(row));
        }
        sharedPositions();
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
                        if (page != R.string.settings_maps || isFinishing()) {
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
        on.setChecked(Core.bool(Core.receipts(null), "on"));
        on.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Map<String, Object> answer = Core.receipts(on.isChecked());
                if (answer.containsKey("error")) {
                    android.widget.Toast.makeText(SettingsActivity.this,
                            Core.str(answer, "error"), android.widget.Toast.LENGTH_LONG).show();
                    on.setChecked(!on.isChecked());
                    return;
                }
                on.setChecked(Core.bool(answer, "on"));
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
        Map<String, Object> answer = Core.shared();
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

    /** The mailbox's backup when it is on this phone. */
    private void backup() {
        boolean phone = Core.bool(Core.me(), "here");
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

    /** The Propagation Node, the map's fling, and the service's end. */
    private void advanced() {
        mapFling(null);
        TextView node = text(getString(R.string.propagation_node), 16, R.color.text);
        node.setPadding(dp(16), dp(16), dp(16), 0);
        body.addView(node);
        body.addView(padded(text(getString(R.string.propagation_node_text), 14,
                R.color.text_secondary)));
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
        TextView stop = text(getString(R.string.stop_service), 16, R.color.text);
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
                Core.block(peer, false);
                refresh();
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
