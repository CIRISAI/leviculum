package network.leviculum.loquax;

import android.app.Activity;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.PopupMenu;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.util.List;
import java.util.Map;

/**
 * The Rooms tab (1384): Reticulum Relay Chat on the hub the config names,
 * as the terminal and the window carry it. A room is drawn as a chat
 * (1450): its bar with the room's avatar, its name and the people seen in
 * it (a tap shows them), a menu with "Leave room"; the lines as bubbles,
 * another's run named once with the writer's avatar, ours on the right,
 * the comings and goings as quiet lines in the middle. Every word is the
 * library's ({@link Core#roomsScreen}); the input field hands its text in
 * whole ({@link Core#rooms} `say`) and the library reads it as the
 * terminal's input line: `#room` joins, a line goes to the open room. A
 * line the hub would not take comes back in the field.
 */
final class RoomsPane {
    /** What the pane hands back to the Activity. */
    interface Host {
        /** The tab gave way to the chats. */
        void left();

        /** The bar's title: "Räume auf EuskalReticulum" with no room open (1659). */
        void title(String words);
    }

    private final Activity screen;
    private final Host host;
    private final TextView status;
    private final TextView retry;
    private final LinearLayout list;
    private final LinearLayout head;
    private final AvatarView face;
    private final TextView name;
    private final TextView subtitle;
    private final ImageButton menu;
    private final ScrollView scroll;
    private final LinearLayout lines;
    private final TextView empty;
    private final EditText input;

    RoomsPane(Activity activity, Host host) {
        this.screen = activity;
        this.host = host;
        FrameLayout pane = (FrameLayout) activity.findViewById(R.id.pane_rooms);
        LinearLayout column = new LinearLayout(activity);
        column.setOrientation(LinearLayout.VERTICAL);
        pane.addView(column, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        // How the link stands, only while it is not up.
        status = new TextView(activity);
        status.setTextColor(activity.getColor(R.color.text_secondary));
        status.setPadding(dp(16), dp(8), dp(16), dp(8));
        status.setTag("rooms_status");
        column.addView(status);
        // "Connect" or "Try again" under it, while the link is neither up
        // nor coming (1652).
        retry = new TextView(activity);
        retry.setTextColor(activity.getColor(R.color.accent));
        retry.setTypeface(null, Typeface.BOLD);
        retry.setPadding(dp(16), dp(4), dp(16), dp(12));
        retry.setTag("rooms_retry");
        retry.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("retry", "", 0);
            }
        });
        column.addView(retry);

        // The open room's bar, a chat's: back, the avatar, the name over
        // the people (a tap shows them), the menu.
        head = new LinearLayout(activity);
        head.setGravity(Gravity.CENTER_VERTICAL);
        head.setPadding(dp(4), 0, dp(4), 0);
        head.setBackgroundColor(activity.getColor(R.color.bar));
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
        head.addView(back, new LinearLayout.LayoutParams(dp(48), dp(48)));
        LinearLayout who = new LinearLayout(activity);
        who.setGravity(Gravity.CENTER_VERTICAL);
        who.setTag("rooms_header");
        who.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("people", "", Core.bool(last, "people_shown") ? 0 : 1);
            }
        });
        face = new AvatarView(activity, null);
        who.addView(face, new LinearLayout.LayoutParams(dp(40), dp(40)));
        LinearLayout words = new LinearLayout(activity);
        words.setOrientation(LinearLayout.VERTICAL);
        words.setPadding(dp(12), 0, 0, 0);
        name = new TextView(activity);
        name.setTypeface(null, Typeface.BOLD);
        name.setTextSize(18);
        name.setTextColor(activity.getColor(R.color.text));
        words.addView(name);
        subtitle = new TextView(activity);
        subtitle.setTextSize(13);
        subtitle.setSingleLine(true);
        subtitle.setEllipsize(android.text.TextUtils.TruncateAt.END);
        subtitle.setTextColor(activity.getColor(R.color.text_secondary));
        subtitle.setTag("rooms_people");
        words.addView(subtitle);
        who.addView(words);
        head.addView(who, new LinearLayout.LayoutParams(0, dp(64), 1));
        menu = new ImageButton(activity);
        menu.setBackground(null);
        menu.setImageDrawable(new Glyph(Glyph.Kind.MENU, activity.getColor(R.color.text)));
        menu.setContentDescription(activity.getString(R.string.scene_menu));
        menu.setTag("rooms_menu");
        menu.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                menu();
            }
        });
        head.addView(menu, new LinearLayout.LayoutParams(dp(48), dp(48)));
        column.addView(head);

        scroll = new ScrollView(activity);
        LinearLayout body = new LinearLayout(activity);
        body.setOrientation(LinearLayout.VERTICAL);
        list = new LinearLayout(activity);
        list.setOrientation(LinearLayout.VERTICAL);
        body.addView(list);
        lines = new LinearLayout(activity);
        lines.setOrientation(LinearLayout.VERTICAL);
        lines.setPadding(dp(8), dp(4), dp(8), dp(8));
        lines.setTag("rooms_lines");
        body.addView(lines);
        empty = new TextView(activity);
        empty.setTextColor(activity.getColor(R.color.text_secondary));
        empty.setGravity(Gravity.CENTER);
        empty.setPadding(dp(24), dp(48), dp(24), dp(24));
        empty.setTag("rooms_empty");
        body.addView(empty);
        scroll.addView(body);
        column.addView(scroll, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout foot = new LinearLayout(activity);
        foot.setGravity(Gravity.CENTER_VERTICAL);
        foot.setPadding(dp(8), dp(4), dp(4), dp(8));
        input = new EditText(activity);
        input.setSingleLine(true);
        input.setImeOptions(EditorInfo.IME_ACTION_SEND);
        input.setTag("rooms_input");
        input.setPadding(dp(16), dp(10), dp(16), dp(10));
        input.setBackground(round(activity.getColor(R.color.bar), dp(24)));
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
        send.setImageDrawable(new Glyph(Glyph.Kind.SEND, activity.getColor(R.color.accent)));
        send.setContentDescription(activity.getString(R.string.send));
        send.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                say();
            }
        });
        foot.addView(send, new LinearLayout.LayoutParams(dp(48), dp(48)));
        column.addView(foot);
    }

    /** The tab came to the front: the library links to the hub once. */
    void show() {
        verb("tab", "", 0);
    }

    /** Back: the people, then the open room to the list; false on the list, as last drawn. */
    boolean back() {
        if (last == null || Core.str(last, "room").isEmpty()) {
            return false;
        }
        if (Core.bool(last, "people_shown")) {
            verb("people", "", 0);
        } else {
            verb("show", "", -1);
        }
        return true;
    }

    /** The room's menu: the library's verbs in its order. */
    private void menu() {
        if (last == null) {
            return;
        }
        final List<Map<String, Object>> verbs = Core.list(last, "menu");
        PopupMenu popup = new PopupMenu(screen, menu);
        for (int i = 0; i < verbs.size(); i++) {
            popup.getMenu().add(Menu.NONE, i, i, Core.str(verbs.get(i), "label"));
        }
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                Map<String, Object> chosen = verbs.get(item.getItemId());
                verb(Core.str(chosen, "verb"), "", (int) Core.num(chosen, "index"));
                return true;
            }
        });
        popup.show();
    }

    /** The field's line to the library; what it leaves in the field back. */
    private void say() {
        verb("say", input.getText().toString(), 0, new Runnable() {
            @Override
            public void run() {
                input.setText(Core.str(last, "input"));
                input.setSelection(input.getText().length());
            }
        });
    }

    private void verb(String verb, String text, int index) {
        verb(verb, text, index, null);
    }

    /** A verb on the engine's thread (1446); the tab drawn after it, then `after`. */
    private void verb(final String verb, final String text, final int index, final Runnable after) {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.rooms(verb, text, index);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                if (error != null) {
                    Toast.makeText(screen, error, Toast.LENGTH_SHORT).show();
                }
                draw(after);
            }
        });
    }

    /** A redraw for the seconds of "Connecting…" is pending (1652). */
    private boolean ticking;

    /** What the tab showed last (1464): Back decides by it. */
    private Map<String, Object> last;

    /** Draw what the library says the tab shows. */
    void draw() {
        draw(null);
    }

    /**
     * Ask the library on the engine's thread (1464), draw its answer, then
     * `after` (null for nothing).
     */
    private void draw(final Runnable after) {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.roomsScreen();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                if (screen.isDestroyed()) {
                    return;
                }
                show(shown);
                if (after != null) {
                    after.run();
                }
            }
        });
    }

    private void show(Map<String, Object> shown) {
        last = shown;
        if (!Core.bool(shown, "open")) {
            host.left();
            return;
        }
        String bar = Core.str(shown, "title");
        if (!bar.isEmpty()) {
            host.title(bar);
        }
        String said = Core.str(shown, "status");
        status.setText(said);
        status.setVisibility(said.isEmpty() ? View.GONE : View.VISIBLE);
        String again = Core.str(shown, "retry");
        retry.setText(again);
        retry.setVisibility(again.isEmpty() ? View.GONE : View.VISIBLE);
        // While it connects the library counts the seconds (1652): ask it
        // again in a second, one tick at a time.
        if (!said.isEmpty() && again.isEmpty() && !ticking) {
            ticking = true;
            status.postDelayed(new Runnable() {
                @Override
                public void run() {
                    ticking = false;
                    if (!screen.isDestroyed()) {
                        draw();
                    }
                }
            }, 1000);
        }
        input.setHint(Core.str(shown, "placeholder"));
        String room = Core.str(shown, "room");
        boolean open = !room.isEmpty();
        head.setVisibility(open ? View.VISIBLE : View.GONE);
        list.setVisibility(open ? View.GONE : View.VISIBLE);
        lines.setVisibility(open ? View.VISIBLE : View.GONE);
        String none = Core.str(shown, "empty");
        empty.setText(none);
        empty.setVisibility(none.isEmpty() ? View.GONE : View.VISIBLE);
        if (!open) {
            list.removeAllViews();
            List<Map<String, Object>> rows = Core.list(shown, "rows");
            for (int i = 0; i < rows.size(); i++) {
                list.addView(row(rows.get(i), i));
            }
            // The rooms the hub names (1652): a tap joins one.
            String title = Core.str(shown, "listed_title");
            if (!title.isEmpty()) {
                // The bar says it already (1659): no second line of it.
                if (!title.equals(bar)) {
                    list.addView(heading(title));
                }
                List<?> named = (List<?>) shown.get("listed");
                if (named != null) {
                    for (int i = 0; i < named.size(); i++) {
                        list.addView(listed(String.valueOf(named.get(i)), i));
                    }
                }
                String note = Core.str(shown, "listed_note");
                if (!note.isEmpty()) {
                    list.addView(note(note));
                }
            }
            String other = Core.str(shown, "other");
            if (!other.isEmpty()) {
                list.addView(heading(other));
            }
            return;
        }
        face.show(room, shown);
        name.setText(room);
        subtitle.setText(Core.str(shown, "subtitle"));
        lines.removeAllViews();
        if (Core.bool(shown, "people_shown")) {
            lines.addView(note(Core.str(shown, "people_note")));
            for (Object who : (List<?>) shown.get("people")) {
                lines.addView(person(String.valueOf(who)));
            }
            return;
        }
        lines.addView(note(Core.str(shown, "live")));
        for (Map<String, Object> line : Core.list(shown, "lines")) {
            lines.addView(Core.bool(line, "bubble") ? bubble(line) : note(Core.str(line, "text")));
        }
        scroll.post(new Runnable() {
            @Override
            public void run() {
                scroll.fullScroll(View.FOCUS_DOWN);
            }
        });
    }

    /** A room of the list: its avatar, its name, the public mark and unread count; a tap opens it. */
    private View row(Map<String, Object> row, final int index) {
        LinearLayout line = new LinearLayout(screen);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(16), dp(10), dp(16), dp(10));
        line.setTag("room_" + index);
        line.setBackgroundResource(android.R.drawable.list_selector_background);
        String name = Core.str(row, "name");
        AvatarView avatar = new AvatarView(screen, null);
        avatar.show(name, row);
        line.addView(avatar, new LinearLayout.LayoutParams(dp(48), dp(48)));
        TextView words = new TextView(screen);
        long unread = Core.num(row, "unread");
        // A room is public (1577): the globe and the word beside the name.
        String open = Core.str(row, "public");
        words.setText(open.isEmpty() ? name : name + "  \uD83C\uDF10 " + open);
        words.setTextSize(17);
        words.setTypeface(null, unread > 0 ? Typeface.BOLD : Typeface.NORMAL);
        words.setTextColor(screen.getColor(R.color.text));
        words.setPadding(dp(16), 0, dp(8), 0);
        line.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        if (unread > 0) {
            TextView badge = new TextView(screen);
            badge.setText(String.valueOf(unread));
            badge.setTextColor(screen.getColor(R.color.badge_text));
            badge.setTextSize(12);
            badge.setPadding(dp(7), dp(2), dp(7), dp(2));
            badge.setBackground(round(screen.getColor(R.color.badge), dp(11)));
            line.addView(badge);
        }
        line.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("show", "", index);
            }
        });
        return line;
    }

    /** A heading over the hub's rooms, or over the field: the library's words. */
    private View heading(String text) {
        TextView view = new TextView(screen);
        view.setText(text);
        view.setTextSize(14);
        view.setTypeface(null, Typeface.BOLD);
        view.setTextColor(screen.getColor(R.color.text_secondary));
        view.setPadding(dp(16), dp(16), dp(16), dp(4));
        return view;
    }

    /** A room the hub names (1652): its avatar and name; a tap joins it and opens it. */
    private View listed(final String name, int index) {
        LinearLayout line = new LinearLayout(screen);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(16), dp(10), dp(16), dp(10));
        line.setTag("rooms_listed_" + index);
        line.setBackgroundResource(android.R.drawable.list_selector_background);
        AvatarView avatar = new AvatarView(screen, null);
        String initial = name.replaceFirst("^#", "");
        avatar.show(name, Core.map("initials",
                initial.isEmpty() ? "" : initial.substring(0, 1).toUpperCase()));
        line.addView(avatar, new LinearLayout.LayoutParams(dp(48), dp(48)));
        TextView words = new TextView(screen);
        words.setText(name);
        words.setTextSize(17);
        words.setTextColor(screen.getColor(R.color.text));
        words.setPadding(dp(16), 0, dp(8), 0);
        line.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        line.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("join", name, 0);
            }
        });
        return line;
    }

    /** A quiet line in the middle: the comings and goings, the hub's words. */
    private View note(String text) {
        TextView view = new TextView(screen);
        view.setText(text);
        view.setTextSize(13);
        view.setGravity(Gravity.CENTER);
        view.setTextColor(screen.getColor(R.color.text_secondary));
        view.setPadding(dp(24), dp(6), dp(24), dp(6));
        return view;
    }

    /** One of the people, with their avatar. */
    private View person(String who) {
        LinearLayout line = new LinearLayout(screen);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(8), dp(6), dp(8), dp(6));
        AvatarView avatar = new AvatarView(screen, null);
        avatar.show(who, Core.map("initials", who.isEmpty() ? "" : who.substring(0, 1).toUpperCase()));
        line.addView(avatar, new LinearLayout.LayoutParams(dp(40), dp(40)));
        TextView name = new TextView(screen);
        name.setText(who);
        name.setTextSize(16);
        name.setTextColor(screen.getColor(R.color.text));
        name.setPadding(dp(16), 0, 0, 0);
        line.addView(name);
        return line;
    }

    /**
     * A bubble: ours on the right in the accent, another's on the left
     * beside the writer's avatar on the first of a run, its name over it;
     * an action in italics with the writer's name in it; the time under
     * the words.
     */
    private View bubble(Map<String, Object> line) {
        boolean ours = Core.bool(line, "ours");
        boolean named = Core.bool(line, "named");
        boolean action = "action".equals(Core.str(line, "kind"));
        LinearLayout side = new LinearLayout(screen);
        side.setGravity(ours ? Gravity.END : Gravity.START);
        side.setPadding(0, named || ours ? dp(8) : dp(2), 0, 0);
        if (!ours) {
            View place;
            if (named) {
                AvatarView avatar = new AvatarView(screen, null);
                avatar.show(Core.str(line, "who"), line);
                place = avatar;
            } else {
                place = new View(screen);
            }
            LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(dp(28), dp(28));
            at.rightMargin = dp(8);
            side.addView(place, at);
        }
        LinearLayout inside = new LinearLayout(screen);
        inside.setOrientation(LinearLayout.VERTICAL);
        inside.setPadding(dp(12), dp(8), dp(12), dp(6));
        inside.setBackground(round(screen.getColor(ours ? R.color.bubble_ours : R.color.bubble_theirs), dp(18)));
        int ink = screen.getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text);
        int meta = screen.getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
        if (named) {
            TextView who = new TextView(screen);
            who.setText(Core.str(line, "who"));
            who.setTextSize(13);
            who.setTextColor(screen.getColor(R.color.accent));
            inside.addView(who);
        }
        TextView words = new TextView(screen);
        words.setText(action ? Core.str(line, "who") + " " + Core.str(line, "text") : Core.str(line, "text"));
        words.setTextSize(16);
        words.setTextColor(ink);
        words.setTextIsSelectable(true);
        if (action) {
            words.setTypeface(null, Typeface.ITALIC);
        }
        inside.addView(words);
        String time = Core.str(line, "time");
        if (!time.isEmpty()) {
            TextView at = new TextView(screen);
            at.setText(time);
            at.setTextSize(11);
            at.setTextColor(meta);
            at.setGravity(Gravity.END);
            inside.addView(at, new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        }
        LinearLayout.LayoutParams wide = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        side.addView(inside, wide);
        inside.setMinimumWidth(dp(64));
        words.setMaxWidth(screen.getResources().getDisplayMetrics().widthPixels * 3 / 4);
        return side;
    }

    /** A round-cornered ground of `colour`. */
    private static GradientDrawable round(int colour, int radius) {
        GradientDrawable ground = new GradientDrawable();
        ground.setColor(colour);
        ground.setCornerRadius(radius);
        return ground;
    }

    private int dp(int value) {
        return Math.round(value * screen.getResources().getDisplayMetrics().density);
    }
}
