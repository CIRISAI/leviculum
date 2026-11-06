package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.view.View;
import android.widget.ImageButton;
import android.widget.TextView;
import java.util.Map;

/**
 * The map as a sub view (1199): opened from a chat or a contact on a
 * message's fix, with its sender's card open (1200), or from a chat's
 * menu. Chat on the card of the friend whose chat opened the map goes
 * back to that chat, where it was.
 * The main views' layout with the back arrow and no tabs; back returns to
 * where the user came from. The map itself is {@link MapPane}'s, the same
 * as the Map tab's.
 */
public class MapActivity extends Activity implements Events.Listener {
    static final String LAT_E6 = "lat_e6";
    static final String LON_E6 = "lon_e6";
    /** The pin to select where the map opens on a fix: its sender's address. */
    static final String SELECTED = "selected";
    /** Open on this friend's newest fix with its card (1224, `OpenMapAt`). */
    static final String FRIEND = "friend";
    /**
     * Open on the place at this zoom, thousandths, with no card: the device
     * walk's way to a view (1193). Only the debug APK exports this screen.
     */
    static final String ZOOM_MILLI = "zoom_milli";

    private MapPane map;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_conversations);
        ConversationsActivity.subView(this);
        ((TextView) findViewById(R.id.title)).setText(R.string.map);
        findViewById(R.id.search).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                map.search();
            }
        });
        ((ImageButton) findViewById(R.id.search)).setImageDrawable(
                new Glyph(Glyph.Kind.SEARCH, getColor(R.color.text)));
        ((ImageButton) findViewById(R.id.search)).setContentDescription(getString(R.string.friends_search));
        findViewById(R.id.pane_chats).setVisibility(View.GONE);
        findViewById(R.id.pane_map).setVisibility(View.VISIBLE);
        final ImageButton menu = (ImageButton) findViewById(R.id.menu);
        menu.setImageDrawable(new Glyph(Glyph.Kind.MENU, getColor(R.color.text)));
        menu.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ConversationsActivity.showMenu(MapActivity.this, menu, map);
            }
        });
        final String from = getIntent().getStringExtra(SELECTED);
        map = new MapPane(this, new MapPane.Host() {
            @Override
            public void chat(String peer) {
                if (peer.equals(from)) {
                    // Opened from this chat: back to it, where it was.
                    finish();
                    return;
                }
                startActivity(new Intent(MapActivity.this, ChatActivity.class)
                        .putExtra(ChatActivity.PEER, peer));
            }

            @Override
            public void title(String said) {
                ((TextView) findViewById(R.id.title)).setText(said);
            }
        });
        if (getIntent().hasExtra(FRIEND)) {
            map.openAt(getIntent().getStringExtra(FRIEND));
        } else if (getIntent().hasExtra(LAT_E6) && getIntent().hasExtra(LON_E6)
                && getIntent().hasExtra(ZOOM_MILLI)) {
            map.centreAt(getIntent().getLongExtra(LAT_E6, 0), getIntent().getLongExtra(LON_E6, 0),
                    getIntent().getLongExtra(ZOOM_MILLI, 0));
        } else if (getIntent().hasExtra(LAT_E6) && getIntent().hasExtra(LON_E6)) {
            map.centreOn(getIntent().getLongExtra(LAT_E6, 0), getIntent().getLongExtra(LON_E6, 0),
                    getIntent().getStringExtra(SELECTED));
        }
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        if (map.permission(request)) {
            return;
        }
        super.onRequestPermissionsResult(request, permissions, results);
    }

    @Override
    public void onBackPressed() {
        if (map.back()) {
            return;
        }
        super.onBackPressed();
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
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.front(true);
            }
        });
        map.ask();
    }

    @Override
    protected void onPause() {
        map.pause();
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.front(false);
            }
        });
        Events.listen(null);
        super.onPause();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        // A position that came moves its pin: the view is drawn anew.
        if ("changed".equals(kind)) {
            map.ask();
            return;
        }
        if ("stopped".equals(kind)) {
            finish();
            return;
        }
        if (!"sync".equals(kind)) {
            map.event(Ui.eventLine(this, event));
        }
    }
}
