package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.app.Dialog;
import android.content.DialogInterface;
import android.view.Gravity;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.LinearLayout;
import android.widget.PopupMenu;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.EditText;
import android.widget.Toast;

import java.util.List;
import java.util.Map;

/**
 * The group screens (1307, the study's section 4) as the platform's
 * dialogs: every word, every row and whether a button may be pressed is
 * the library's ({@link Core#groupScreen}), every press one verb
 * ({@link Core#group}). "New group" ticks friends, then asks the name with
 * the trust sentence under it; the member sheet lists the members with
 * their state, the admin's Remove on a row, Leave at the bottom, each
 * asked once with the library's question, and the admin's Add members,
 * which opens the picker for the group (1307 G2); an invitation is the
 * card's words and Join.
 */
final class GroupDialogs {
    private GroupDialogs() {}

    private static void say(Activity screen, String error) {
        if (error != null) {
            Toast.makeText(screen, error, Toast.LENGTH_LONG).show();
        }
    }

    /**
     * One verb of {@link Core#group} on the engine's thread (1446); its
     * refusal said, then `after` on the UI thread, which reads the sheet
     * the verb left.
     */
    private static void verb(final Activity screen, final String verb, final String group,
            final String member, final String name, final boolean sure, final Runnable after) {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.group(verb, group, member, name, sure);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                say(screen, error);
                if (after != null) {
                    after.run();
                }
            }
        });
    }

    /** {@link #verb} for `group`, with no member, name or confirmation. */
    private static void verb(Activity screen, String verb, String group, Runnable after) {
        verb(screen, verb, group, "", "", false, after);
    }

    /**
     * The group screen as the library has it now, asked on the engine's
     * thread (1464), to `then` on the UI thread while `screen` lives.
     */
    private static void shown(final Activity screen, final Engine.Got<Map<String, Object>> then) {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.groupScreen();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                if (!screen.isDestroyed()) {
                    then.got(shown);
                }
            }
        });
    }

    /** The open sheet closes. */
    private static void close(Activity screen) {
        verb(screen, "close", "", null);
    }

    /** "New group": the picker, then {@link #name}. */
    static void newGroup(final Activity screen) {
        verb(screen, "new", "", new Runnable() {
            @Override
            public void run() {
                picker(screen, false);
            }
        });
    }

    /**
     * "Add members" (1307 G2): the same picker for `group`, its Next is
     * "Add" and asks the home; the answer closes it.
     */
    static void addMembers(final Activity screen, final String group) {
        verb(screen, "add", group, new Runnable() {
            @Override
            public void run() {
                picker(screen, true);
            }
        });
    }

    /**
     * The people to tick, then Next (a new group's name step) or Add: the new conversation's list
     * with check marks (1448, UI walk 1 finding 17), not a dialog of names. On that screen it turns
     * into the picker; from another screen (a group's Add members) it opens as one.
     */
    private static void picker(final Activity screen, final boolean adding) {
        if (screen instanceof NewConversationActivity) {
            ((NewConversationActivity) screen).pick(adding);
            return;
        }
        screen.startActivity(new android.content.Intent(screen, NewConversationActivity.class)
                .putExtra(NewConversationActivity.PICK, true)
                .putExtra(NewConversationActivity.ADDING, adding));
    }

    /** A person ticked or unticked in the picker; `after` reads the screen the verb left. */
    static void pick(Activity screen, String peer, Runnable after) {
        verb(screen, "pick", peer, after);
    }

    /** The picker given up: the draft closes. */
    static void cancel(Activity screen) {
        close(screen);
    }

    /**
     * The picker's button: Next asks the name of a new group, Add asks the home (whose answer
     * opens the member sheet). `over` runs when the picker is done with: after Add, after a name
     * step given up, or when the library has no name step to show.
     */
    static void next(final Activity screen, final boolean adding, final Runnable over) {
        verb(screen, "next", "", new Runnable() {
            @Override
            public void run() {
                if (adding) {
                    over.run();
                    return;
                }
                shown(screen, new Engine.Got<Map<String, Object>>() {
                    @Override
                    public void got(Map<String, Object> now) {
                        if ("name".equals(Core.str(now, "step"))) {
                            name(screen, now, over);
                        } else {
                            close(screen);
                            over.run();
                        }
                    }
                });
            }
        });
    }

    /** The name step: the field, the trust sentence, Create. */
    private static void name(final Activity screen) {
        shown(screen, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                name(screen, shown, null);
            }
        });
    }

    /** {@link #name} as the library `shown` it; `cancelled`, if any, runs when it is given up. */
    private static void name(final Activity screen, Map<String, Object> shown,
            final Runnable cancelled) {
        final EditText field = new EditText(screen);
        field.setHint(Core.str(shown, "hint"));
        // The draft's name: empty for a new group, the old one when it
        // starts again (1307 pass four).
        field.setText(Core.str(shown, "name"));
        field.setSingleLine(true);
        new AlertDialog.Builder(screen)
                .setTitle(Core.str(shown, "title"))
                .setMessage(Core.str(shown, "trust"))
                .setView(field)
                .setPositiveButton(Core.str(shown, "go"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        verb(screen, "name", "", "", field.getText().toString(), false, null);
                        // The home answers; the list shows the group and
                        // the chat opens when it does.
                        verb(screen, "create", "", new Runnable() {
                            @Override
                            public void run() {
                                screen.finish();
                            }
                        });
                    }
                })
                .setNegativeButton(android.R.string.cancel, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        close(screen);
                        if (cancelled != null) {
                            cancelled.run();
                        }
                    }
                })
                .show();
    }

    /** The invitation's card for `group`: its words, the trust sentence, Join. */
    static void invitation(final Activity screen, final String group, String title, String trust,
            final Runnable after) {
        new AlertDialog.Builder(screen)
                .setTitle(title)
                .setMessage(trust)
                .setPositiveButton(R.string.group_join, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        verb(screen, "join", group, after);
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    /** The member sheet of `group`. */
    static void members(final Activity screen, final String group, final Runnable after) {
        verb(screen, "members", group, new Runnable() {
            @Override
            public void run() {
                memberSheet(screen, group, after);
            }
        });
    }

    /** The member sheet the library opened for `group`. */
    private static void memberSheet(final Activity screen, final String group, final Runnable after) {
        shown(screen, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                memberSheet(screen, group, after, shown);
            }
        });
    }

    /**
     * {@link #memberSheet} as the library `shown` it: a screen of its own,
     * as the window's members screen draws it (1449, walk 1 finding 9). A
     * row is the member's avatar, its name (two of one name told apart by
     * their hash tails, in the library) and its role on the right; a tap on
     * a row the admin may remove opens the row's menu with Remove, which
     * asks before it acts, and a tap on any other does nothing. Add and
     * "Start again" sit under the list; Leave and Delete red at the bottom.
     */
    private static void memberSheet(final Activity screen, final String group, final Runnable after,
            Map<String, Object> shown) {
        final float dp = screen.getResources().getDisplayMetrics().density;
        final Dialog dialog = new Dialog(screen, android.R.style.Theme_DeviceDefault_Light_NoActionBar);
        LinearLayout page = new LinearLayout(screen);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setBackgroundColor(screen.getColor(R.color.background));
        int pad = (int) (16 * dp);
        page.setPadding(pad, pad, pad, pad);

        LinearLayout head = new LinearLayout(screen);
        head.setOrientation(LinearLayout.HORIZONTAL);
        head.setGravity(Gravity.CENTER_VERTICAL);
        LinearLayout titles = new LinearLayout(screen);
        titles.setOrientation(LinearLayout.VERTICAL);
        titles.addView(text(screen, Core.str(shown, "title"), 20, R.color.text));
        titles.addView(text(screen, Core.str(shown, "line"), 14, R.color.text_secondary));
        head.addView(titles, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        TextView close = text(screen, screen.getString(R.string.scene_close), 16, R.color.accent);
        close.setPadding(pad, pad / 2, 0, pad / 2);
        close.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                dialog.dismiss();
            }
        });
        head.addView(close);
        page.addView(head);

        LinearLayout list = new LinearLayout(screen);
        list.setOrientation(LinearLayout.VERTICAL);
        for (final Map<String, Object> row : Core.list(shown, "rows")) {
            LinearLayout line = new LinearLayout(screen);
            line.setOrientation(LinearLayout.HORIZONTAL);
            line.setGravity(Gravity.CENTER_VERTICAL);
            line.setMinimumHeight((int) (56 * dp));
            AvatarView avatar = new AvatarView(screen, null);
            avatar.show(Core.str(row, "peer"), row);
            LinearLayout.LayoutParams side = new LinearLayout.LayoutParams((int) (40 * dp), (int) (40 * dp));
            side.setMarginEnd(pad);
            line.addView(avatar, side);
            TextView name = text(screen, Core.str(row, "name"), 16, R.color.text);
            name.setSingleLine(true);
            name.setEllipsize(android.text.TextUtils.TruncateAt.END);
            line.addView(name, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
            line.addView(text(screen, Core.str(row, "state"), 14, R.color.text_secondary));
            final String remove = Core.str(row, "remove");
            if (!remove.isEmpty()) {
                line.setBackgroundResource(android.R.drawable.list_selector_background);
                line.setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        PopupMenu menu = new PopupMenu(screen, v, Gravity.END);
                        menu.getMenu().add(remove);
                        menu.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
                            @Override
                            public boolean onMenuItemClick(MenuItem item) {
                                final String member = Core.str(row, "peer");
                                dialog.setOnDismissListener(null);
                                dialog.dismiss();
                                verb(screen, "remove", group, member, "", false, new Runnable() {
                                    @Override
                                    public void run() {
                                        ask(screen, "remove", group, member, after);
                                    }
                                });
                                return true;
                            }
                        });
                        menu.show();
                    }
                });
            }
            list.addView(line);
        }
        ScrollView scroll = new ScrollView(screen);
        scroll.addView(list);
        page.addView(scroll, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        String add = Core.str(shown, "add");
        if (!add.isEmpty()) {
            page.addView(action(screen, add, R.color.accent, new Runnable() {
                @Override
                public void run() {
                    dialog.setOnDismissListener(null);
                    dialog.dismiss();
                    addMembers(screen, group);
                }
            }));
        }
        // "Start again with the same people" (study 4.7), where Add never
        // is: a group the reader left or that ended. "New group" opens at
        // its name, the people ticked; its Create makes it.
        String again = Core.str(shown, "again");
        if (!again.isEmpty() && add.isEmpty()) {
            page.addView(action(screen, again, R.color.accent, new Runnable() {
                @Override
                public void run() {
                    dialog.setOnDismissListener(null);
                    dialog.dismiss();
                    verb(screen, "again", group, new Runnable() {
                        @Override
                        public void run() {
                            name(screen);
                        }
                    });
                }
            }));
        }
        String leave = Core.str(shown, "leave");
        if (!leave.isEmpty()) {
            page.addView(action(screen, leave, R.color.failed_mark_on_panel, new Runnable() {
                @Override
                public void run() {
                    dialog.setOnDismissListener(null);
                    dialog.dismiss();
                    verb(screen, "leave", group, new Runnable() {
                        @Override
                        public void run() {
                            ask(screen, "leave", group, "", after);
                        }
                    });
                }
            }));
        }
        // The admin's "Delete group" (1307 pass four): red at the bottom,
        // never a row of the member list.
        String delete = Core.str(shown, "delete");
        if (!delete.isEmpty()) {
            page.addView(action(screen, delete, R.color.failed_mark_on_panel, new Runnable() {
                @Override
                public void run() {
                    dialog.setOnDismissListener(null);
                    dialog.dismiss();
                    verb(screen, "delete", group, new Runnable() {
                        @Override
                        public void run() {
                            ask(screen, "delete", group, "", after);
                        }
                    });
                }
            }));
        }
        dialog.setContentView(page);
        dialog.setOnDismissListener(new DialogInterface.OnDismissListener() {
            @Override
            public void onDismiss(DialogInterface d) {
                close(screen);
            }
        });
        dialog.show();
        if (dialog.getWindow() != null) {
            dialog.getWindow().setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT);
            dialog.getWindow().setBackgroundDrawable(
                    new android.graphics.drawable.ColorDrawable(screen.getColor(R.color.background)));
        }
    }

    /** A line of text in `size` sp and the colour `colour`. */
    private static TextView text(Activity screen, String words, int size, int colour) {
        TextView view = new TextView(screen);
        view.setText(words);
        view.setTextSize(size);
        view.setTextColor(screen.getColor(colour));
        view.setVisibility(words.isEmpty() ? View.GONE : View.VISIBLE);
        return view;
    }

    /** A text button under the list, in `colour`. */
    private static TextView action(Activity screen, String words, int colour, final Runnable tapped) {
        TextView view = text(screen, words, 16, colour);
        int pad = (int) (12 * screen.getResources().getDisplayMetrics().density);
        view.setPadding(0, pad, 0, pad);
        view.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                tapped.run();
            }
        });
        return view;
    }

    /** The sheet's question before a Leave or a Remove, in the library's words. */
    private static void ask(final Activity screen, final String verb, final String group,
            final String member, final Runnable after) {
        shown(screen, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                ask(screen, verb, group, member, after, shown);
            }
        });
    }

    /** {@link #ask} as the library `shown` it. */
    private static void ask(final Activity screen, final String verb, final String group,
            final String member, final Runnable after, Map<String, Object> shown) {
        new AlertDialog.Builder(screen)
                .setMessage(Core.str(shown, "ask"))
                .setPositiveButton(Core.str(shown, "yes"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        GroupDialogs.verb(screen, verb, group, member, "", true, null);
                        GroupDialogs.verb(screen, "close", "", after);
                    }
                })
                .setNegativeButton(Core.str(shown, "no"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        close(screen);
                        close(screen);
                    }
                })
                .show();
    }
}
