package network.leviculum.loquax;

import android.app.Activity;
import android.os.Bundle;
import android.view.View;
import android.widget.ImageButton;
import android.widget.TextView;
import java.util.Map;

/**
 * The map as a sub view (1199): opened from a chat or a contact on a
 * message's fix, with its sender's pin selected, or from a chat's menu.
 * The main views' layout with the back arrow and no tabs; back returns to
 * where the user came from. The map itself is {@link MapPane}'s, the same
 * as the Map tab's.
 */
public class MapActivity extends Activity implements Events.Listener {
    static final String LAT_E6 = "lat_e6";
    static final String LON_E6 = "lon_e6";
    /** The pin to select where the map opens on a fix: its sender's address. */
    static final String SELECTED = "selected";

    private MapPane map;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_conversations);
        ConversationsActivity.subView(this);
        ((TextView) findViewById(R.id.title)).setText(R.string.map);
        findViewById(R.id.search).setVisibility(View.GONE);
        findViewById(R.id.pane_chats).setVisibility(View.GONE);
        findViewById(R.id.pane_map).setVisibility(View.VISIBLE);
        final ImageButton menu = (ImageButton) findViewById(R.id.menu);
        menu.setImageDrawable(new Glyph(Glyph.Kind.MENU, getColor(R.color.text)));
        menu.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ConversationsActivity.showMenu(MapActivity.this, menu);
            }
        });
        map = new MapPane(this);
        if (getIntent().hasExtra(LAT_E6) && getIntent().hasExtra(LON_E6)) {
            map.centreOn(getIntent().getLongExtra(LAT_E6, 0), getIntent().getLongExtra(LON_E6, 0),
                    getIntent().getStringExtra(SELECTED));
        }
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
        Core.front(true);
        map.ask();
    }

    @Override
    protected void onPause() {
        Core.front(false);
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
