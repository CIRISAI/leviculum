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

    /** "New group": the picker, then {@link #name}. */
    static void newGroup(final Activity screen) {
        say(screen, Core.group("new", "", "", "", false));
        picker(screen, false);
    }

    /**
     * "Add members" (1307 G2): the same picker for `group`, its Next is
     * "Add" and asks the home; the answer closes it.
     */
    static void addMembers(final Activity screen, final String group) {
        say(screen, Core.group("add", group, "", "", false));
        picker(screen, true);
    }

    /** The friends to tick, then Next (a new group's name step) or Add. */
    private static void picker(final Activity screen, final boolean adding) {
        final Map<String, Object> shown = Core.groupScreen();
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
                        say(screen, Core.group("pick", Core.str(rows.get(which), "peer"), "", "", false));
                    }
                })
                .setPositiveButton(Core.str(shown, "go"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        say(screen, Core.group("next", "", "", "", false));
                        if (adding) {
                            // The home is asked; its answer closes the
                            // picker and opens the member sheet.
                            return;
                        }
                        if ("name".equals(Core.str(Core.groupScreen(), "step"))) {
                            name(screen);
                        } else {
                            Core.group("close", "", "", "", false);
                        }
                    }
                })
                .setNegativeButton(android.R.string.cancel, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        Core.group("close", "", "", "", false);
                    }
                })
                .show();
    }

    /** The name step: the field, the trust sentence, Create. */
    private static void name(final Activity screen) {
        Map<String, Object> shown = Core.groupScreen();
        final EditText field = new EditText(screen);
        field.setHint(Core.str(shown, "hint"));
        field.setSingleLine(true);
        new AlertDialog.Builder(screen)
                .setTitle(Core.str(shown, "title"))
                .setMessage(Core.str(shown, "trust"))
                .setView(field)
                .setPositiveButton(Core.str(shown, "go"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        say(screen, Core.group("name", "", "", field.getText().toString(), false));
                        say(screen, Core.group("create", "", "", "", false));
                        // The home answers; the list shows the group and
                        // the chat opens when it does.
                        screen.finish();
                    }
                })
                .setNegativeButton(android.R.string.cancel, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        Core.group("close", "", "", "", false);
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
                        say(screen, Core.group("join", group, "", "", false));
                        after.run();
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    /** The member sheet of `group`. */
    static void members(final Activity screen, final String group, final Runnable after) {
        say(screen, Core.group("members", group, "", "", false));
        final Map<String, Object> shown = Core.groupScreen();
        final List<Map<String, Object>> rows = Core.list(shown, "rows");
        String[] lines = new String[rows.size()];
        for (int i = 0; i < rows.size(); i++) {
            Map<String, Object> row = rows.get(i);
            String remove = Core.str(row, "remove");
            lines[i] = Core.str(row, "name") + " · " + Core.str(row, "state")
                    + (remove.isEmpty() ? "" : "  (" + remove + ")");
        }
        AlertDialog.Builder dialog = new AlertDialog.Builder(screen)
                .setTitle(Core.str(shown, "title") + " · " + Core.str(shown, "line"))
                .setItems(lines, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        Map<String, Object> row = rows.get(which);
                        if (Core.str(row, "remove").isEmpty()) {
                            Core.group("close", "", "", "", false);
                            return;
                        }
                        String member = Core.str(row, "peer");
                        say(screen, Core.group("remove", group, member, "", false));
                        ask(screen, "remove", group, member, after);
                    }
                })
                .setNegativeButton(android.R.string.cancel, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        Core.group("close", "", "", "", false);
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
        String leave = Core.str(shown, "leave");
        if (!leave.isEmpty()) {
            dialog.setPositiveButton(leave, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int which) {
                    say(screen, Core.group("leave", group, "", "", false));
                    ask(screen, "leave", group, "", after);
                }
            });
        }
        dialog.show();
    }

    /** The sheet's question before a Leave or a Remove, in the library's words. */
    private static void ask(final Activity screen, final String verb, final String group,
            final String member, final Runnable after) {
        Map<String, Object> shown = Core.groupScreen();
        new AlertDialog.Builder(screen)
                .setMessage(Core.str(shown, "ask"))
                .setPositiveButton(Core.str(shown, "yes"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        say(screen, Core.group(verb, group, member, "", true));
                        Core.group("close", "", "", "", false);
                        after.run();
                    }
                })
                .setNegativeButton(Core.str(shown, "no"), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        Core.group("close", "", "", "", false);
                        Core.group("close", "", "", "", false);
                    }
                })
                .show();
    }
}
