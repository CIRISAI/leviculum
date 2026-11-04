package network.leviculum.loquax;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.InputMethodManager;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.ListView;
import android.widget.PopupMenu;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * The main views (1199): the chat list and the map side by side under a
 * bottom tab bar, one top bar over both: our avatar with the status dot
 * (green while the mailbox link is up), which opens our profile (1187),
 * the tab's name, search and a menu
 * of Settings and Help. The selected tab survives a rotation; system back
 * on the map goes to the chats first.
 *
 * The conversation list, after a modern messenger's overview (1130): each row an avatar,
 * the name, two lines of the last message, the time, and the unread count
 * or our last message's delivery mark; newest activity first, as the
 * library orders it, again on every `changed`; a round button for a new one.
 *
 * A long press on a row (1146), after Signal's, marks it and opens its
 * menu: read or unread, pin, mute, archive, delete. The archived
 * conversations are this screen again with ARCHIVED, a sub view with the
 * back arrow and no tabs, reached from the list's last row.
 *
 * A row whose peer has a position younger than a day carries the map pin
 * (1224, the library's `pin`): a tap on it is the core's `OpenMapAt`, the
 * Map tab centred on that friend with the friend's card open, and Back
 * returns to the chats, not to the map's previous view. A chat's newest
 * report raises the same through {@link #MAP_AT}.
 */
public class ConversationsActivity extends Activity implements Events.Listener {
    /** The extra that makes this screen the archived view. */
    static final String ARCHIVED = "archived";
    /** The extra that opens the Map tab on a friend (1224): its address. */
    static final String MAP_AT = "map_at";
    /**
     * The extra with a friend's code from outside (walk finding 12): an
     * lxma:// link tapped in another app, or a text shared to Loquax that
     * carries one. "Add contact" asks the name over the list.
     */
    static final String ADD_CODE = "add_code";
    /** The saved state's key of the selected tab. */
    private static final String TAB = "tab";
    /** Every row the library listed, and those the search leaves. */
    private final List<Map<String, Object>> all = new ArrayList<>();
    private final List<Map<String, Object>> rows = new ArrayList<>();
    private TextView status;
    private TextView title;
    private EditText query;
    private ImageButton search;
    private AvatarView own;
    private Adapter adapter;
    private boolean archivedView;
    private boolean homeClient;
    /** The map tab is selected; its pane is made on first selection. */
    private boolean onMap;
    /** The Browse tab is in front (1337), and its pane once built. */
    private boolean onBrowse;
    private BrowsePane browsePane;
    /** The Rooms tab is in front (1384), and its pane once built. */
    private boolean onRooms;
    private RoomsPane roomsPane;
    private MapPane mapPane;
    /** The map was opened on a friend from a pin: Back goes to the chats. */
    private boolean mapFromPin;
    /** The list's last row: the archived view, with how many. */
    private TextView archivedRow;
    private final ServiceLink link = new ServiceLink(this);
    /**
     * "Jetzt" turns into "1 Min." without a message: the rows are asked
     * again twice a minute, the library's words (a position's age) with them.
     */
    private final Handler ticker = new Handler(Looper.getMainLooper());
    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            refresh();
            ticker.postDelayed(this, 30_000);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_conversations);
        status = (TextView) findViewById(R.id.status);
        title = (TextView) findViewById(R.id.title);
        query = (EditText) findViewById(R.id.query);
        own = (AvatarView) findViewById(R.id.avatar);
        archivedView = getIntent().getBooleanExtra(ARCHIVED, false);
        int text = getColor(R.color.text);
        own.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(ConversationsActivity.this, ProfileActivity.class));
            }
        });
        ListView list = (ListView) findViewById(R.id.list);
        archivedRow = new TextView(this);
        archivedRow.setTextColor(getColor(R.color.text_secondary));
        archivedRow.setTextSize(15);
        archivedRow.setGravity(android.view.Gravity.CENTER);
        int pad = Math.round(16 * getResources().getDisplayMetrics().density);
        archivedRow.setPadding(pad, pad, pad, pad);
        archivedRow.setVisibility(View.GONE);
        list.addFooterView(archivedRow, null, true);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setEmptyView(findViewById(R.id.empty));
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                if (position >= rows.size()) {
                    archived();
                    return;
                }
                chat(Core.str(rows.get(position), "peer"));
            }
        });
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                if (position >= rows.size()) {
                    return false;
                }
                rowMenu(view, rows.get(position));
                return true;
            }
        });
        // Before the tabs: selecting one sets the search icon.
        search = (ImageButton) findViewById(R.id.search);
        if (archivedView) {
            title.setText(R.string.archived);
            ((TextView) findViewById(R.id.empty)).setText(R.string.archived_none);
            findViewById(R.id.write).setVisibility(View.GONE);
            subView(this);
        } else {
            tabs(savedInstanceState != null && savedInstanceState.getBoolean(TAB, false));
        }
        if (savedInstanceState == null) {
            mapAt(getIntent());
            addCode(getIntent());
        }
        search.setImageDrawable(new Glyph(Glyph.Kind.SEARCH, text));
        search.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                if (onMap) {
                    mapPane.search();
                    return;
                }
                searching(query.getVisibility() != View.VISIBLE);
            }
        });
        query.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                filter();
            }
        });
        final ImageButton menu = (ImageButton) findViewById(R.id.menu);
        menu.setImageDrawable(new Glyph(Glyph.Kind.MENU, text));
        menu.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showMenu(menu);
            }
        });
        ImageButton write = (ImageButton) findViewById(R.id.write);
        write.setImageDrawable(new Glyph(Glyph.Kind.PENCIL, getColor(R.color.on_send)));
        write.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(ConversationsActivity.this, NewConversationActivity.class));
            }
        });
        Asks.once(this);
    }

    /**
     * The layout as a sub view (1199): the back arrow in the avatar's
     * place, no tab bar. The archive and the map opened from a chat.
     */
    static void subView(final Activity screen) {
        screen.findViewById(R.id.avatar).setVisibility(View.GONE);
        screen.findViewById(R.id.tabs).setVisibility(View.GONE);
        ImageButton back = (ImageButton) screen.findViewById(R.id.back);
        back.setVisibility(View.VISIBLE);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, screen.getColor(R.color.text)));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                screen.finish();
            }
        });
    }

    /** The tab bar: Chats and Map, `map` selected at the start. */
    private void tabs(boolean map) {
        findViewById(R.id.tab_chats).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                mapFromPin = false;
                select(false);
            }
        });
        findViewById(R.id.tab_map).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                mapFromPin = false;
                select(true);
            }
        });
        findViewById(R.id.tab_browse).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                browse();
            }
        });
        tint(R.id.tab_browse_icon, R.id.tab_browse_label, Glyph.Kind.SEARCH, false);
        // The tab shows where the library has it (1337 pass two): off in the
        // APK until its fetch through the phone's own node was seen on a device.
        findViewById(R.id.tab_browse).setVisibility(
                Core.bool(Core.browseScreen(), "enabled") ? View.VISIBLE : View.GONE);
        findViewById(R.id.tab_rooms).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                rooms();
            }
        });
        tint(R.id.tab_rooms_icon, R.id.tab_rooms_label, Glyph.Kind.HASH, false);
        // The tab shows where the config names a hub (1384), as on the
        // terminal and the window: the library says so.
        findViewById(R.id.tab_rooms).setVisibility(
                Core.bool(Core.roomsScreen(), "enabled") ? View.VISIBLE : View.GONE);
        select(map);
    }

    /**
     * The Rooms tab (1384): Reticulum Relay Chat on the config's hub, its
     * pane drawn by {@link RoomsPane} from the library's surface. Chats or
     * Map leaves it.
     */
    private void rooms() {
        if (query.getVisibility() == View.VISIBLE) {
            searching(false);
        }
        if (onMap && mapPane != null) {
            mapPane.pause();
        }
        leaveBrowse();
        onMap = false;
        onRooms = true;
        if (roomsPane == null) {
            roomsPane = new RoomsPane(this, new RoomsPane.Host() {
                @Override
                public void left() {
                    if (onRooms) {
                        select(false);
                    }
                }
            });
        }
        findViewById(R.id.pane_chats).setVisibility(View.GONE);
        findViewById(R.id.pane_map).setVisibility(View.GONE);
        findViewById(R.id.pane_rooms).setVisibility(View.VISIBLE);
        title.setText(R.string.tab_rooms);
        tint(R.id.tab_chats_icon, R.id.tab_chats_label, Glyph.Kind.CHAT, false);
        tint(R.id.tab_map_icon, R.id.tab_map_label, Glyph.Kind.PIN, false);
        tint(R.id.tab_rooms_icon, R.id.tab_rooms_label, Glyph.Kind.HASH, true);
        findViewById(R.id.tab_chats).setSelected(false);
        findViewById(R.id.tab_map).setSelected(false);
        findViewById(R.id.tab_rooms).setSelected(true);
        roomsPane.show();
    }

    /** The Rooms tab gives way, if it is in front. */
    private void leaveRooms() {
        if (onRooms) {
            onRooms = false;
            Core.rooms("chats", "", 0);
            findViewById(R.id.pane_rooms).setVisibility(View.GONE);
            tint(R.id.tab_rooms_icon, R.id.tab_rooms_label, Glyph.Kind.HASH, false);
            findViewById(R.id.tab_rooms).setSelected(false);
        }
    }

    /** The Browse tab gives way, if it is in front. */
    private void leaveBrowse() {
        if (onBrowse) {
            onBrowse = false;
            Core.browse("chats", "", "");
            findViewById(R.id.pane_browse).setVisibility(View.GONE);
            tint(R.id.tab_browse_icon, R.id.tab_browse_label, Glyph.Kind.SEARCH, false);
            findViewById(R.id.tab_browse).setSelected(false);
        }
    }

    /**
     * The Browse tab (1337): lnomad's browser, its pane drawn by
     * {@link BrowsePane} from the library's scene. Chats or Map leaves it.
     */
    private void browse() {
        if (query.getVisibility() == View.VISIBLE) {
            searching(false);
        }
        if (onMap && mapPane != null) {
            mapPane.pause();
        }
        leaveRooms();
        onMap = false;
        onBrowse = true;
        if (browsePane == null) {
            browsePane = new BrowsePane(this, new BrowsePane.Host() {
                @Override
                public void left() {
                    if (onBrowse) {
                        select(false);
                    }
                }
            });
        }
        findViewById(R.id.pane_chats).setVisibility(View.GONE);
        findViewById(R.id.pane_map).setVisibility(View.GONE);
        findViewById(R.id.pane_browse).setVisibility(View.VISIBLE);
        title.setText(R.string.tab_browse);
        tint(R.id.tab_chats_icon, R.id.tab_chats_label, Glyph.Kind.CHAT, false);
        tint(R.id.tab_map_icon, R.id.tab_map_label, Glyph.Kind.PIN, false);
        tint(R.id.tab_browse_icon, R.id.tab_browse_label, Glyph.Kind.SEARCH, true);
        findViewById(R.id.tab_chats).setSelected(false);
        findViewById(R.id.tab_map).setSelected(false);
        findViewById(R.id.tab_browse).setSelected(true);
        browsePane.show();
    }

    /** Show the map (`true`) or the chats, each pane keeping its state. */
    private void select(boolean map) {
        if (map && query.getVisibility() == View.VISIBLE) {
            searching(false);
        }
        onMap = map;
        leaveBrowse();
        leaveRooms();
        if (map && mapPane == null) {
            mapPane = new MapPane(this, new MapPane.Host() {
                @Override
                public void chat(String peer) {
                    ConversationsActivity.this.chat(peer);
                }

                @Override
                public void title(String said) {
                    if (onMap) {
                        title.setText(said);
                    }
                }
            });
        }
        findViewById(R.id.pane_chats).setVisibility(map ? View.GONE : View.VISIBLE);
        findViewById(R.id.pane_map).setVisibility(map ? View.VISIBLE : View.GONE);
        if (map && mapPane.title() != null) {
            // The time window's word after it when not the default (1419).
            title.setText(mapPane.title());
        } else {
            title.setText(map ? R.string.map : R.string.tab_chats);
        }
        // On the map the search finds friends, in the Friends sheet (1200).
        search.setImageDrawable(new Glyph(Glyph.Kind.SEARCH, getColor(R.color.text)));
        search.setContentDescription(getString(map ? R.string.friends_search : R.string.search));
        tint(R.id.tab_chats_icon, R.id.tab_chats_label, Glyph.Kind.CHAT, !map);
        tint(R.id.tab_map_icon, R.id.tab_map_label, Glyph.Kind.PIN, map);
        findViewById(R.id.tab_chats).setSelected(!map);
        findViewById(R.id.tab_map).setSelected(map);
        if (map) {
            mapPane.ask();
        } else if (mapPane != null) {
            // The map's position is read only while it is shown (1201).
            mapPane.pause();
        }
    }

    /**
     * The core's `OpenMapAt` (1224): the Map tab centred on `peer`'s
     * newest fix with its card open; Back returns to the chats. The
     * archived view has no tabs: the map opens as a sub view there.
     */
    private void openMapAt(String peer) {
        if (archivedView) {
            startActivity(new Intent(this, MapActivity.class).putExtra(MapActivity.FRIEND, peer));
            return;
        }
        if (query.getVisibility() == View.VISIBLE) {
            searching(false);
        }
        select(true);
        mapPane.openAt(peer);
        mapFromPin = true;
    }

    /** A screen asked for the map on a friend with {@link #MAP_AT}. */
    private void mapAt(Intent intent) {
        String peer = intent == null ? null : intent.getStringExtra(MAP_AT);
        if (peer != null && !peer.isEmpty()) {
            intent.removeExtra(MAP_AT);
            openMapAt(peer);
        }
    }

    /** A friend's code from outside, {@link #ADD_CODE}: the name asked, the chat opened. */
    private void addCode(Intent intent) {
        String code = intent == null ? null : intent.getStringExtra(ADD_CODE);
        if (code != null && !code.isEmpty()) {
            intent.removeExtra(ADD_CODE);
            String why = AddContact.take(this, code, null);
            if (why != null) {
                android.widget.Toast.makeText(this, why, android.widget.Toast.LENGTH_LONG).show();
            }
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        mapAt(intent);
        addCode(intent);
    }

    /**
     * Raise the core's `OpenMapAt` from another screen (the chat's newest
     * report, 1224): the main views come to the top with the Map tab on
     * `peer`, the screens above them closed, so Back goes to the chats.
     */
    static void openMapAt(Activity from, String peer) {
        from.startActivity(new Intent(from, ConversationsActivity.class)
                .putExtra(MAP_AT, peer)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP));
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        if (mapPane != null && mapPane.permission(request)) {
            return;
        }
        super.onRequestPermissionsResult(request, permissions, results);
    }

    /** One tab's icon and label, in the accent when selected. */
    private void tint(int icon, int label, Glyph.Kind kind, boolean selected) {
        int colour = getColor(selected ? R.color.accent : R.color.text_secondary);
        ((ImageView) findViewById(icon)).setImageDrawable(new Glyph(kind, colour));
        TextView words = (TextView) findViewById(label);
        words.setTextColor(colour);
        words.setTypeface(null, selected ? android.graphics.Typeface.BOLD : android.graphics.Typeface.NORMAL);
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putBoolean(TAB, onMap);
    }

    /**
     * The bar's menu (1199): Settings and Help; on the map its Time
     * window and Fit all first (1419).
     */
    private void showMenu(View anchor) {
        showMenu(this, anchor, onMap ? mapPane : null);
    }

    /** The main views' menu, on `screen`'s bar; with `map`, the map's entries first. */
    static void showMenu(final Activity screen, View anchor, final MapPane map) {
        PopupMenu popup = new PopupMenu(screen, anchor);
        if (map != null) {
            MapPane.menu(popup.getMenu());
        }
        popup.getMenu().add(0, R.string.settings, 1, R.string.settings);
        popup.getMenu().add(0, R.string.help, 2, R.string.help);
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                if (map != null && map.chosen(item.getItemId())) {
                    return true;
                }
                if (item.getItemId() == R.string.help) {
                    // The user help page in the app's language (walk
                    // finding 18): help_url is /help, /help-de in German.
                    String help = screen.getString(R.string.help_url);
                    try {
                        screen.startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(help)));
                    } catch (android.content.ActivityNotFoundException e) {
                        android.widget.Toast.makeText(screen, help, android.widget.Toast.LENGTH_LONG).show();
                    }
                } else {
                    screen.startActivity(new Intent(screen, SettingsActivity.class));
                }
                return true;
            }
        });
        popup.show();
    }

    private void archived() {
        startActivity(new Intent(this, ConversationsActivity.class).putExtra(ARCHIVED, true));
    }

    /**
     * A row's menu, after Signal's long press: the row marked while it is
     * open; read or unread, pin, mute, archive, delete.
     */
    private void rowMenu(final View view, Map<String, Object> row) {
        final String peer = Core.str(row, "peer");
        final String name = Core.str(row, "name");
        final boolean unread = Core.num(row, "unread") > 0;
        final boolean pinned = Core.bool(row, "pinned");
        final boolean muted = Core.bool(row, "muted");
        view.setBackgroundColor(getColor(R.color.selected_row));
        PopupMenu popup = new PopupMenu(this, view, android.view.Gravity.END);
        popup.getMenu().add(0, R.string.mark_read, 0, unread ? R.string.mark_read : R.string.mark_unread);
        if (!archivedView) {
            popup.getMenu().add(0, R.string.pin, 1, pinned ? R.string.unpin : R.string.pin);
        }
        popup.getMenu().add(0, R.string.mute, 2, muted ? R.string.unmute : R.string.mute);
        popup.getMenu().add(0, R.string.archive, 3, archivedView ? R.string.unarchive : R.string.archive);
        popup.getMenu().add(0, R.string.delete, 4, R.string.delete);
        popup.setOnDismissListener(new PopupMenu.OnDismissListener() {
            @Override
            public void onDismiss(PopupMenu menu) {
                view.setBackgroundColor(0);
            }
        });
        final Runnable again = new Runnable() {
            @Override
            public void run() {
                refresh();
            }
        };
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                int id = item.getItemId();
                if (id == R.string.mark_read) {
                    say(Core.unread(peer, !unread));
                } else if (id == R.string.pin) {
                    say(Core.pin(peer, !pinned));
                } else if (id == R.string.mute) {
                    ContactActivity.muteDialog(ConversationsActivity.this, peer, muted, again);
                } else if (id == R.string.archive) {
                    say(Core.archive(peer, !archivedView));
                } else if (id == R.string.delete) {
                    ContactActivity.deleteDialog(ConversationsActivity.this, peer, name,
                            homeClient, again);
                }
                refresh();
                return true;
            }
        });
        popup.show();
    }

    /** The search field in the bar's place of the name, or the name again. */
    private void searching(boolean on) {
        InputMethodManager keyboard = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        title.setVisibility(on ? View.GONE : View.VISIBLE);
        query.setVisibility(on ? View.VISIBLE : View.GONE);
        search.setImageDrawable(new Glyph(on ? Glyph.Kind.CLOSE : Glyph.Kind.SEARCH, getColor(R.color.text)));
        search.setContentDescription(getString(on ? R.string.close_search : R.string.search));
        if (on) {
            query.requestFocus();
            keyboard.showSoftInput(query, InputMethodManager.SHOW_IMPLICIT);
        } else {
            query.setText("");
            keyboard.hideSoftInputFromWindow(query.getWindowToken(), 0);
        }
    }

    @Override
    public void onBackPressed() {
        if (query.getVisibility() == View.VISIBLE) {
            searching(false);
            return;
        }
        // An open room's back to the list, then the chats (1384).
        if (onRooms) {
            if (!roomsPane.back()) {
                select(false);
            }
            return;
        }
        // The browser's back while it has one, then the chats (1337).
        if (onBrowse) {
            if (!browsePane.back()) {
                select(false);
            }
            return;
        }
        if (onMap && mapFromPin) {
            // Opened on a friend from a pin (1224): back to the chats, the
            // card closed with it.
            mapFromPin = false;
            mapPane.back();
            select(false);
            return;
        }
        if (onMap && mapPane.back()) {
            return;
        }
        if (onMap) {
            select(false);
            return;
        }
        super.onBackPressed();
    }

    private void chat(String peer) {
        startActivity(new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, peer));
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
        if (Ui.NOT_STARTED.equals(Core.close())) {
            // Back from the recent apps after the session was stopped: the
            // first screen starts it again.
            startActivity(new Intent(this, MainActivity.class));
            finish();
            return;
        }
        Core.front(true);
        refresh();
        if (onMap) {
            mapPane.ask();
        }
        ticker.postDelayed(tick, 30_000);
    }

    @Override
    protected void onPause() {
        if (mapPane != null) {
            mapPane.pause();
        }
        ticker.removeCallbacks(tick);
        Core.front(false);
        Events.listen(null);
        super.onPause();
    }

    private void say(String error) {
        if (error != null) {
            line(error);
        }
    }

    /** The line under the bar, shown only while it has something to say. */
    private void line(String text) {
        Ui.showLine(status, text, true);
    }

    private void refresh() {
        Map<String, Object> answer = archivedView ? Core.archived(null) : Core.conversations(null);
        if (answer.containsKey("error")) {
            line(Core.str(answer, "error"));
            return;
        }
        own.show(Core.str(answer, "address"), answer);
        if (!archivedView) {
            boolean up = Core.bool(answer, "home_link");
            own.dot(up);
            own.setContentDescription(getString(up ? R.string.mailbox_connected : R.string.mailbox_not_connected));
        }
        homeClient = Core.bool(answer, "home_client");
        long archived = Core.num(answer, "archived");
        archivedRow.setVisibility(!archivedView && archived > 0 ? View.VISIBLE : View.GONE);
        archivedRow.setText(getString(R.string.archived_count, archived));
        all.clear();
        all.addAll(Core.list(answer, "conversations"));
        filter();
        if (!archivedView) {
            badge();
        }
    }

    /** The Chats tab's badge: how many chats hold unread messages. */
    private void badge() {
        int chats = 0;
        for (Map<String, Object> row : all) {
            if (Core.num(row, "unread") > 0) {
                chats++;
            }
        }
        TextView badge = (TextView) findViewById(R.id.tab_chats_badge);
        badge.setVisibility(chats > 0 ? View.VISIBLE : View.GONE);
        badge.setText(String.valueOf(chats));
        findViewById(R.id.tab_chats).setContentDescription(chats > 0
                ? getString(R.string.tab_chats_unread, chats) : getString(R.string.tab_chats));
    }

    /**
     * The rows the search field finds, by the library's rule (1217): every
     * word in the name or the last line, as on the desktop. The whole list
     * while the field is empty.
     */
    private void filter() {
        String needle = query.getText().toString().trim();
        rows.clear();
        if (needle.isEmpty()) {
            rows.addAll(all);
        } else {
            Map<String, Object> found = archivedView ? Core.archived(needle) : Core.conversations(needle);
            rows.addAll(Core.list(found, "conversations"));
        }
        adapter.notifyDataSetChanged();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            refresh();
            if (onRooms) {
                // A line the hub brought, or how the link stands.
                roomsPane.draw();
            }
            if (onMap) {
                // A position that came moves its pin.
                mapPane.ask();
            }
        } else if ("stopped".equals(kind)) {
            // The notification's stop button, the menu's, or the engine
            // gone: the session is over, and so is the app until it starts
            // again.
            finish();
        } else {
            String line = Ui.eventLine(this, event);
            if (onMap) {
                // The map shows what happened as a chip; a pull's progress
                // is the list's business.
                if (!"sync".equals(kind)) {
                    mapPane.event(line);
                }
            } else if (line != null) {
                Ui.showLine(status, line, Ui.stays(event));
            }
        }
    }

    private final class Adapter extends BaseAdapter {
        @Override
        public int getCount() {
            return rows.size();
        }

        @Override
        public Object getItem(int position) {
            return rows.get(position);
        }

        @Override
        public long getItemId(int position) {
            return position;
        }

        @Override
        public View getView(int position, View view, ViewGroup parent) {
            if (view == null) {
                view = getLayoutInflater().inflate(R.layout.row_conversation, parent, false);
            }
            Map<String, Object> row = rows.get(position);
            long unread = Core.num(row, "unread");
            TextView name = (TextView) view.findViewById(R.id.name);
            // Pinned and muted marks after the name (1146).
            String marks = (Core.bool(row, "pinned") ? " \uD83D\uDCCC" : "")
                    + (Core.bool(row, "muted") ? " \uD83D\uDD15" : "");
            name.setText(Core.str(row, "name") + marks);
            name.setTypeface(null, unread > 0 ? android.graphics.Typeface.BOLD : android.graphics.Typeface.NORMAL);
            // A message typed and not sent stands in for the last one, in
            // italics, in the library's words (1219): `Draft: …`.
            String draft = Core.str(row, "draft");
            TextView last = (TextView) view.findViewById(R.id.last);
            last.setText(draft.isEmpty() ? Ui.preview(ConversationsActivity.this, row) : draft);
            last.setTypeface(null, draft.isEmpty()
                    ? android.graphics.Typeface.NORMAL : android.graphics.Typeface.ITALIC);
            TextView at = (TextView) view.findViewById(R.id.at);
            at.setText(Ui.listTime(ConversationsActivity.this, Core.num(row, "at"), Ui.nowSecs()));
            at.setTextColor(getColor(unread > 0 ? R.color.accent : R.color.text_secondary));
            final String peer = Core.str(row, "peer");
            ((AvatarView) view.findViewById(R.id.avatar)).show(peer, row);
            // Our share runs here (1273).
            view.findViewById(R.id.sharing).setVisibility(
                    Core.bool(row, "sharing") ? View.VISIBLE : View.GONE);
            // The map pin (1224) in the library's tint: the map on the peer.
            String tint = Core.str(row, "pin");
            ImageView pin = (ImageView) view.findViewById(R.id.pin);
            pin.setVisibility(tint.isEmpty() ? View.GONE : View.VISIBLE);
            if (!tint.isEmpty()) {
                int colour = getColor("accent".equals(tint) ? R.color.accent : R.color.text_secondary);
                pin.setImageDrawable(new Glyph(Glyph.Kind.PIN, colour));
                pin.setContentDescription(Core.str(row, "pin_label"));
                pin.setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        openMapAt(peer);
                    }
                });
            } else {
                pin.setOnClickListener(null);
                pin.setClickable(false);
            }
            TextView badge = (TextView) view.findViewById(R.id.unread);
            badge.setVisibility(unread > 0 ? View.VISIBLE : View.GONE);
            badge.setText(String.valueOf(unread));
            // Under the time either the count or, ours being the last, its
            // mark: the ticks cut into the list's ground.
            String state = Core.bool(row, "ours") ? Core.str(row, "state") : "";
            ImageView mark = (ImageView) view.findViewWithTag("mark");
            mark.setVisibility(unread == 0 && !state.isEmpty() ? View.VISIBLE : View.GONE);
            if (!state.isEmpty()) {
                Glyph.Kind kind = Glyph.mark(state);
                int ink = getColor(kind == Glyph.Kind.TICKS_READ ? R.color.accent : R.color.text_secondary);
                mark.setImageDrawable(new Glyph(kind, ink, getColor(R.color.list_failed_ground),
                        getColor(R.color.list_failed_mark)));
                mark.setId(Glyph.resource(kind));
                mark.setContentDescription(Ui.state(ConversationsActivity.this, state, Core.num(row, "read_at")));
            }
            return view;
        }
    }
}
