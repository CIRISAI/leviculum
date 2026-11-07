package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
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

    /** The friends to tick, then Next (a new group's name step) or Add. */
    private static void picker(final Activity screen, final boolean adding) {
        shown(screen, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                picker(screen, adding, shown);
            }
        });
    }

    /** {@link #picker} as the library `shown` it. */
    private static void picker(final Activity screen, final boolean adding,
            final Map<String, Object> shown) {
        final List<Map<String, Object>> rows = Core.list(shown, "rows");
        String[] names = new String[rows.size()];
        boolean[] picked = new boolean[rows.size()];
        for (int i = 0; i < rows.size(); i++) {
            names[i] = Core.str(rows.get(i), "name");
            picked[i] = Core.bool(rows.get(i), "picked");
        }
        new AlertDialog.Builder(screen)
                .setTitle(Core.str(shown, "title"))
                .setMultiChoiceItems(names, picked, new DialogInterface.OnMultiChoiceClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which, boolean on) {
                        verb(screen, "pick", Core.str(rows.get(which), "peer"), null);
                    }
                })
                .setPositiveButton(Core.str(shown, "go"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        verb(screen, "next", "", new Runnable() {
                            @Override
                            public void run() {
                                if (adding) {
                                    // The home is asked; its answer closes the
                                    // picker and opens the member sheet.
                                    return;
                                }
                                shown(screen, new Engine.Got<Map<String, Object>>() {
                                    @Override
                                    public void got(Map<String, Object> now) {
                                        if ("name".equals(Core.str(now, "step"))) {
                                            name(screen, now);
                                        } else {
                                            close(screen);
                                        }
                                    }
                                });
                            }
                        });
                    }
                })
                .setNegativeButton(android.R.string.cancel, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        close(screen);
                    }
                })
                .show();
    }

    /** The name step: the field, the trust sentence, Create. */
    private static void name(final Activity screen) {
        shown(screen, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> shown) {
                name(screen, shown);
            }
        });
    }

    /** {@link #name} as the library `shown` it. */
    private static void name(final Activity screen, Map<String, Object> shown) {
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

    /** {@link #memberSheet} as the library `shown` it. */
    private static void memberSheet(final Activity screen, final String group, final Runnable after,
            Map<String, Object> shown) {
        final List<Map<String, Object>> rows = Core.list(shown, "rows");
        // The admin's "Delete group" (1307 pass four) is the list's last
        // line: the dialog's three buttons are taken by Cancel, Add and
        // Leave.
        final String delete = Core.str(shown, "delete");
        String[] lines = new String[rows.size() + (delete.isEmpty() ? 0 : 1)];
        for (int i = 0; i < rows.size(); i++) {
            Map<String, Object> row = rows.get(i);
            String remove = Core.str(row, "remove");
            lines[i] = Core.str(row, "name") + " · " + Core.str(row, "state")
                    + (remove.isEmpty() ? "" : "  (" + remove + ")");
        }
        if (!delete.isEmpty()) {
            lines[rows.size()] = delete;
        }
        AlertDialog.Builder dialog = new AlertDialog.Builder(screen)
                .setTitle(Core.str(shown, "title") + " · " + Core.str(shown, "line"))
                .setItems(lines, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        if (which == rows.size()) {
                            verb(screen, "delete", group, new Runnable() {
                                @Override
                                public void run() {
                                    ask(screen, "delete", group, "", after);
                                }
                            });
                            return;
                        }
                        Map<String, Object> row = rows.get(which);
                        if (Core.str(row, "remove").isEmpty()) {
                            close(screen);
                            return;
                        }
                        final String member = Core.str(row, "peer");
                        verb(screen, "remove", group, member, "", false,
                                new Runnable() {
                                    @Override
                                    public void run() {
                                        ask(screen, "remove", group, member, after);
                                    }
                                });
                    }
                })
                .setNegativeButton(android.R.string.cancel, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        close(screen);
                    }
                });
        String add = Core.str(shown, "add");
        if (!add.isEmpty()) {
            dialog.setNeutralButton(add, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int which) {
                    addMembers(screen, group);
                }
            });
        }
        // "Start again with the same people" (study 4.7), where Add never
        // is: a group the reader left or that ended. "New group" opens at
        // its name, the people ticked; its Create makes it.
        String again = Core.str(shown, "again");
        if (!again.isEmpty() && add.isEmpty()) {
            dialog.setNeutralButton(again, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int which) {
                    verb(screen, "again", group, new Runnable() {
                        @Override
                        public void run() {
                            name(screen);
                        }
                    });
                }
            });
        }
        String leave = Core.str(shown, "leave");
        if (!leave.isEmpty()) {
            dialog.setPositiveButton(leave, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int which) {
                    verb(screen, "leave", group, new Runnable() {
                        @Override
                        public void run() {
                            ask(screen, "leave", group, "", after);
                        }
                    });
                }
            });
        }
        dialog.show();
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
