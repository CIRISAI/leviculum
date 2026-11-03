package network.leviculum.loquax;

import android.app.Activity;
import android.graphics.Typeface;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.style.ForegroundColorSpan;
import android.text.style.StyleSpan;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
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
 * The Rooms tab (1384): Reticulum Relay Chat on the hub the config names,
 * as the terminal and the window carry it. Every word, the hub's line,
 * the rooms, the open room's lines and what the input does are the
 * library's ({@link Core#roomsScreen}); the input field hands its text in
 * whole ({@link Core#rooms} `say`) and the library reads it as the
 * terminal's input line: `#room` joins, a line goes to the open room,
 * `/me` acts, `/part` leaves. A line the hub would not take comes back in
 * the field.
 */
final class RoomsPane {
    /** What the pane hands back to the Activity. */
    interface Host {
        /** The tab gave way to the chats. */
        void left();
    }

    private final Activity screen;
    private final Host host;
    private final TextView hub;
    private final LinearLayout list;
    private final LinearLayout head;
    private final TextView name;
    private final TextView live;
    private final ScrollView scroll;
    private final TextView lines;
    private final EditText input;

    RoomsPane(Activity activity, Host host) {
        this.screen = activity;
        this.host = host;
        FrameLayout pane = (FrameLayout) activity.findViewById(R.id.pane_rooms);
        LinearLayout column = new LinearLayout(activity);
        column.setOrientation(LinearLayout.VERTICAL);
        pane.addView(column, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        hub = new TextView(activity);
        hub.setTextColor(activity.getColor(R.color.text_secondary));
        hub.setPadding(16, 8, 16, 8);
        hub.setTag("rooms_hub");
        column.addView(hub);

        // The open room's head: back to the list, its name, and the one
        // line that says what a room is not.
        head = new LinearLayout(activity);
        head.setOrientation(LinearLayout.VERTICAL);
        LinearLayout bar = new LinearLayout(activity);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        ImageButton back = new ImageButton(activity);
        back.setBackground(null);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, activity.getColor(R.color.text)));
        back.setContentDescription(activity.getString(R.string.back));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("show", "", -1);
            }
        });
        bar.addView(back);
        name = new TextView(activity);
        name.setTypeface(null, Typeface.BOLD);
        name.setTextColor(activity.getColor(R.color.text));
        bar.addView(name);
        head.addView(bar);
        live = new TextView(activity);
        live.setTextColor(activity.getColor(R.color.text_secondary));
        live.setTypeface(null, Typeface.ITALIC);
        live.setPadding(16, 0, 16, 8);
        live.setTag("rooms_live");
        head.addView(live);
        column.addView(head);

        scroll = new ScrollView(activity);
        LinearLayout body = new LinearLayout(activity);
        body.setOrientation(LinearLayout.VERTICAL);
        list = new LinearLayout(activity);
        list.setOrientation(LinearLayout.VERTICAL);
        body.addView(list);
        lines = new TextView(activity);
        lines.setTextColor(activity.getColor(R.color.text));
        lines.setPadding(16, 8, 16, 8);
        lines.setTextIsSelectable(true);
        lines.setTag("rooms_lines");
        body.addView(lines);
        scroll.addView(body);
        column.addView(scroll, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout foot = new LinearLayout(activity);
        foot.setGravity(Gravity.CENTER_VERTICAL);
        input = new EditText(activity);
        input.setSingleLine(true);
        input.setImeOptions(EditorInfo.IME_ACTION_SEND);
        input.setTag("rooms_input");
        input.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                say();
                return true;
            }
        });
        foot.addView(input, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        ImageButton send = new ImageButton(activity);
        send.setBackground(null);
        send.setImageDrawable(new Glyph(Glyph.Kind.SEND, activity.getColor(R.color.text)));
        send.setContentDescription(activity.getString(R.string.send));
        send.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                say();
            }
        });
        foot.addView(send);
        column.addView(foot);
    }

    /** The tab came to the front: the library links to the hub once. */
    void show() {
        verb("tab", "", 0);
    }

    /** Back: from an open room to the list; false on the list. */
    boolean back() {
        if (Core.str(Core.roomsScreen(), "room").isEmpty()) {
            return false;
        }
        verb("show", "", -1);
        return true;
    }

    /** The field's line to the library; what it leaves in the field back. */
    private void say() {
        verb("say", input.getText().toString(), 0);
        input.setText(Core.str(Core.roomsScreen(), "input"));
        input.setSelection(input.getText().length());
    }

    private void verb(String verb, String text, int index) {
        String error = Core.rooms(verb, text, index);
        if (error != null) {
            Toast.makeText(screen, error, Toast.LENGTH_SHORT).show();
        }
        draw();
    }

    /** Draw what the library says the tab shows. */
    void draw() {
        Map<String, Object> shown = Core.roomsScreen();
        if (!Core.bool(shown, "open")) {
            host.left();
            return;
        }
        hub.setText(Core.str(shown, "hub"));
        input.setHint(Core.str(shown, "hint"));
        String room = Core.str(shown, "room");
        boolean open = !room.isEmpty();
        head.setVisibility(open ? View.VISIBLE : View.GONE);
        list.setVisibility(open ? View.GONE : View.VISIBLE);
        lines.setVisibility(open ? View.VISIBLE : View.GONE);
        if (!open) {
            list.removeAllViews();
            List<Map<String, Object>> rows = Core.list(shown, "rows");
            for (int i = 0; i < rows.size(); i++) {
                list.addView(row(rows.get(i), i));
            }
            return;
        }
        name.setText(room);
        live.setText(Core.str(shown, "live"));
        SpannableStringBuilder text = new SpannableStringBuilder();
        for (Map<String, Object> line : Core.list(shown, "lines")) {
            append(text, line);
        }
        lines.setText(text);
        scroll.post(new Runnable() {
            @Override
            public void run() {
                scroll.fullScroll(View.FOCUS_DOWN);
            }
        });
    }

    /** A room of the list: its name and unread count; a tap opens it. */
    private View row(Map<String, Object> row, final int index) {
        TextView view = new TextView(screen);
        long unread = Core.num(row, "unread");
        String name = Core.str(row, "name");
        view.setText(unread > 0 ? name + "  (" + unread + ")" : name);
        view.setTypeface(null, unread > 0 ? Typeface.BOLD : Typeface.NORMAL);
        view.setTextColor(screen.getColor(R.color.text));
        view.setPadding(16, 16, 16, 16);
        view.setTag("room_" + index);
        view.setBackgroundResource(android.R.drawable.list_selector_background);
        view.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("show", "", index);
            }
        });
        return view;
    }

    /**
     * One line: a nick in bold and what it said; an action as `* nick
     * acts`; the comings and goings and the hub's own lines in the quiet
     * colour, as the terminal draws them.
     */
    private void append(SpannableStringBuilder text, Map<String, Object> line) {
        int flags = Spanned.SPAN_EXCLUSIVE_EXCLUSIVE;
        String kind = Core.str(line, "kind");
        String who = Core.str(line, "who");
        int start = text.length();
        if ("action".equals(kind)) {
            text.append("* ").append(who).append(' ').append(Core.str(line, "text"));
            text.setSpan(new StyleSpan(Typeface.ITALIC), start, text.length(), flags);
        } else if (who.isEmpty()) {
            text.append("— ").append(Core.str(line, "text"));
            text.setSpan(new ForegroundColorSpan(screen.getColor(R.color.text_secondary)),
                    start, text.length(), flags);
        } else {
            text.append(who);
            text.setSpan(new StyleSpan(Typeface.BOLD), start, text.length(), flags);
            text.append("  ").append(Core.str(line, "text"));
        }
        text.append('\n');
    }
}
