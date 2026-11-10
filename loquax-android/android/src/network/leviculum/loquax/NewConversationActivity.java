package network.leviculum.loquax;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.os.Bundle;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * A new conversation, from the list's pencil (1130, 1263; 1448, UI walk 1
 * finding 17), as a messenger's contact list: the search field, "New
 * contact" and "New group" on top, then every person by name with their
 * picture; a tap opens the chat. "New contact" unfolds the ways to add one:
 * "Scan a code" and "Paste a code", both read by the library
 * (loquax::code) as every scanner is, and typing an address under
 * Advanced. A friend's code, a contact code (`lxma://`, 1187) or an
 * address alone adds the contact under the name asked for and opens the
 * chat; a mailbox's code goes to the join; anything else is said in one
 * sentence under the rows.
 *
 * The group picker (1307) is the same screen with check marks
 * ({@link GroupDialogs#picker}): the library says who may be picked and
 * who is, {@link PeopleRows} gives each the look of the list, and the
 * button at the bottom is the library's Next or Add.
 */
public class NewConversationActivity extends Activity {
    private static final int SCAN = 1;
    private static final String ADVANCED_OPEN = "advanced";
    /** The extra that opens the screen as the group picker; {@link #ADDING} for a group's Add members. */
    static final String PICK = "pick";
    static final String ADDING = "adding";

    private EditText address;
    private EditText search;
    private TextView error;
    private TextView advanced;
    private View advancedBody;
    private View contactBody;
    private ListView list;
    private TextView none;
    private Button go;
    private final List<Map<String, Object>> rows = new ArrayList<>();
    private final Adapter adapter = new Adapter();
    /** The group picker: the screen's rows carry check marks. */
    private boolean picking;
    private boolean adding;
    /** The picker was the screen's first use, so its end ends the screen. */
    private boolean pickOnly;
    /** The screen is gone or going: an answer that comes after draws nothing. */
    private boolean over;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_new);
        address = (EditText) findViewById(R.id.address);
        search = (EditText) findViewById(R.id.new_search);
        error = (TextView) findViewById(R.id.error);
        say(null);
        advanced = (TextView) findViewById(R.id.new_advanced);
        advancedBody = findViewById(R.id.new_advanced_body);
        contactBody = findViewById(R.id.new_contact_body);
        list = (ListView) findViewById(R.id.new_people);
        none = (TextView) findViewById(R.id.new_none);
        go = (Button) findViewById(R.id.new_go);
        list.setAdapter(adapter);
        ImageButton back = (ImageButton) findViewById(R.id.back);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, getColor(R.color.text)));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                onBackPressed();
            }
        });
        findViewById(R.id.new_contact).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                contactBody.setVisibility(contactBody.getVisibility() == View.VISIBLE
                        ? View.GONE : View.VISIBLE);
            }
        });
        View scan = findViewById(R.id.new_scan);
        if (ScanActivity.available(this)) {
            scan.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startActivityForResult(new Intent(NewConversationActivity.this, ScanActivity.class)
                            .putExtra(ScanActivity.PREFIX, ScanActivity.CODES), SCAN);
                }
            });
        } else {
            scan.setVisibility(View.GONE);
        }
        findViewById(R.id.new_group).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                GroupDialogs.newGroup(NewConversationActivity.this);
            }
        });
        findViewById(R.id.new_paste).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                paste();
            }
        });
        advanced.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showAdvanced(advancedBody.getVisibility() != View.VISIBLE);
            }
        });
        boolean open = savedInstanceState != null && savedInstanceState.getBoolean(ADVANCED_OPEN);
        if (open) {
            contactBody.setVisibility(View.VISIBLE);
        }
        showAdvanced(open);
        ((Button) findViewById(R.id.open)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                take(address.getText().toString());
            }
        });
        address.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                if (action == EditorInfo.IME_ACTION_GO) {
                    take(address.getText().toString());
                    return true;
                }
                return false;
            }
        });
        search.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                refresh();
            }
        });
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                tapped(position);
            }
        });
        go.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                go.setEnabled(false);
                GroupDialogs.next(NewConversationActivity.this, adding, new Runnable() {
                    @Override
                    public void run() {
                        endPick();
                    }
                });
            }
        });
        if (getIntent().getBooleanExtra(PICK, false)) {
            pickOnly = true;
            pick(getIntent().getBooleanExtra(ADDING, false));
        } else {
            refresh();
        }
    }

    @Override
    protected void onDestroy() {
        over = true;
        super.onDestroy();
    }

    @Override
    public void onBackPressed() {
        if (picking) {
            // The picker given up: the library closes its draft.
            GroupDialogs.cancel(this);
            endPick();
            return;
        }
        super.onBackPressed();
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putBoolean(ADVANCED_OPEN, advancedBody.getVisibility() == View.VISIBLE);
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
    protected void onActivityResult(int request, int result, Intent data) {
        if (request == SCAN && result == RESULT_OK && data != null) {
            take(String.valueOf(data.getStringExtra(ScanActivity.TEXT)));
        }
    }

    /**
     * The screen as the group picker (1307 via {@link GroupDialogs}): the same list with check
     * marks, the top rows gone, the library's title and button. `adding` for a group's Add members.
     */
    void pick(boolean adding) {
        this.picking = true;
        this.adding = adding;
        findViewById(R.id.new_top).setVisibility(View.GONE);
        go.setVisibility(View.VISIBLE);
        go.setEnabled(true);
        refresh();
    }

    /** The picker is over: the screen it opened as ends, the list's screen shows its list again. */
    private void endPick() {
        if (pickOnly || !picking) {
            finish();
            return;
        }
        picking = false;
        findViewById(R.id.new_top).setVisibility(View.VISIBLE);
        ((TextView) findViewById(R.id.new_title)).setText(R.string.new_conversation);
        go.setVisibility(View.GONE);
        refresh();
    }

    /** The rows again, as the library has them now, for what the search field holds. */
    private void refresh() {
        final String needle = search.getText().toString().trim();
        final boolean picker = picking;
        Engine.ask(new Engine.Ask<Object[]>() {
            @Override
            public Object[] ask() {
                Map<String, Object> people = Core.people(needle.isEmpty() ? null : needle);
                return new Object[] {people, picker ? Core.groupScreen() : null};
            }
        }, new Engine.Got<Object[]>() {
            @Override
            @SuppressWarnings("unchecked")
            public void got(Object[] answer) {
                if (over || picker != picking
                        || !needle.equals(search.getText().toString().trim())) {
                    return;
                }
                List<Map<String, Object>> people =
                        Core.list((Map<String, Object>) answer[0], "conversations");
                rows.clear();
                if (picker) {
                    Map<String, Object> shown = (Map<String, Object>) answer[1];
                    List<Map<String, Object>> candidates = Core.list(shown, "rows");
                    if (!needle.isEmpty()) {
                        // The search narrows the candidates to those it finds.
                        candidates = within(candidates, people);
                    }
                    rows.addAll(PeopleRows.pickable(people, candidates));
                    ((TextView) findViewById(R.id.new_title)).setText(Core.str(shown, "title"));
                    go.setText(Core.str(shown, "go"));
                } else {
                    rows.addAll(people);
                }
                none.setVisibility(rows.isEmpty() && needle.isEmpty() ? View.VISIBLE : View.GONE);
                adapter.notifyDataSetChanged();
            }
        });
    }

    /** The candidates whose peer is among `found`. */
    private static List<Map<String, Object>> within(List<Map<String, Object>> candidates,
            List<Map<String, Object>> found) {
        List<Map<String, Object>> kept = new ArrayList<>();
        for (Map<String, Object> candidate : candidates) {
            for (Map<String, Object> row : found) {
                if (Core.str(row, "peer").equals(Core.str(candidate, "peer"))) {
                    kept.add(candidate);
                    break;
                }
            }
        }
        return kept;
    }

    /** A row tapped: its chat, or in the picker its check mark. */
    private void tapped(int position) {
        if (position < 0 || position >= rows.size()) {
            return;
        }
        Map<String, Object> row = rows.get(position);
        String peer = Core.str(row, "peer");
        if (picking) {
            // Shown at once, the library told; the next refresh is its word.
            row.put("picked", !Core.bool(row, "picked"));
            adapter.notifyDataSetChanged();
            GroupDialogs.pick(this, peer, new Runnable() {
                @Override
                public void run() {
                    refresh();
                }
            });
            return;
        }
        startActivity(new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, peer));
        finish();
    }

    /** Advanced open with the address field, or collapsed. */
    private void showAdvanced(boolean open) {
        advanced.setText(getString(R.string.advanced) + (open ? "  ▴" : "  ▾"));
        advancedBody.setVisibility(open ? View.VISIBLE : View.GONE);
        if (open) {
            address.requestFocus();
        }
    }

    /** The clipboard's text, taken as a code. */
    private void paste() {
        ClipboardManager clipboard = getSystemService(ClipboardManager.class);
        ClipData clip = clipboard == null ? null : clipboard.getPrimaryClip();
        CharSequence text = clip == null || clip.getItemCount() == 0
                ? null : clip.getItemAt(0).coerceToText(this);
        if (text == null || text.toString().trim().isEmpty()) {
            say(getString(R.string.new_nothing));
            return;
        }
        take(text.toString());
    }

    /** A code scanned, pasted or typed: the friend added, or why not under the rows. */
    private void take(String text) {
        if (text.trim().isEmpty()) {
            return;
        }
        String why = AddContact.take(this, text, new Runnable() {
            @Override
            public void run() {
                finish();
            }
        });
        say(why);
    }

    /**
     * Why a code was not taken, under the rows; with nothing to say the line
     * takes no room (1629, walk 3 new 10: an empty line stood as a gap
     * before Advanced).
     */
    private void say(CharSequence why) {
        boolean said = why != null && why.length() > 0;
        error.setText(said ? why : "");
        error.setVisibility(said ? View.VISIBLE : View.GONE);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    /** The people as rows: the avatar, the name, and in the picker its check mark. */
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
            return Core.str(rows.get(position), "peer").hashCode();
        }

        @Override
        public boolean hasStableIds() {
            return true;
        }

        @Override
        public View getView(int position, View convertView, ViewGroup parent) {
            LinearLayout row = (LinearLayout) convertView;
            if (row == null) {
                row = new LinearLayout(NewConversationActivity.this);
                row.setOrientation(LinearLayout.HORIZONTAL);
                row.setGravity(Gravity.CENTER_VERTICAL);
                row.setMinimumHeight(dp(64));
                row.setPadding(dp(16), dp(8), dp(16), dp(8));
                AvatarView avatar = new AvatarView(NewConversationActivity.this, null);
                row.addView(avatar, new LinearLayout.LayoutParams(dp(44), dp(44)));
                TextView name = new TextView(NewConversationActivity.this);
                name.setTextSize(17);
                name.setTextColor(getColor(R.color.text));
                name.setSingleLine(true);
                name.setEllipsize(android.text.TextUtils.TruncateAt.END);
                LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(
                        0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
                at.leftMargin = dp(16);
                row.addView(name, at);
                CheckBox mark = new CheckBox(NewConversationActivity.this);
                // The row takes the tap; the box only shows it.
                mark.setClickable(false);
                mark.setFocusable(false);
                row.addView(mark, new LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
            }
            Map<String, Object> shown = rows.get(position);
            String peer = Core.str(shown, "peer");
            ((AvatarView) row.getChildAt(0)).show(peer, shown);
            ((TextView) row.getChildAt(1)).setText(Core.str(shown, "name"));
            CheckBox mark = (CheckBox) row.getChildAt(2);
            mark.setVisibility(picking ? View.VISIBLE : View.GONE);
            mark.setChecked(Core.bool(shown, "picked"));
            row.setContentDescription(Core.str(shown, "name"));
            return row;
        }
    }
}
