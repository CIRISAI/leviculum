package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.content.Intent;
import android.text.InputType;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.Toast;
import java.util.Map;

/**
 * A friend from their code (1187, 1263): a contact code
 * `lxma://<address>:<key>`, the code Columba and Sideband share, or an
 * address alone, the first start's old QR. Scanned or pasted on the
 * pencil's screen or the profile. The library reads it (loquax::code) and
 * says whose it is; the user names it; the library keeps the name, hands
 * the key to the mailbox and opens the conversation, which this opens on
 * screen. A mailbox's code goes to the join.
 */
final class AddContact {
    private static final String PREFIX = "lxma://";

    private AddContact() {}

    /** Whether `text` is meant as a contact code. */
    static boolean is(String text) {
        return text != null && text.trim().regionMatches(true, 0, PREFIX, 0, PREFIX.length());
    }

    /**
     * Take the code `text`: a friend's asks the name, adds them and opens
     * their chat, then runs `then` (null for nothing); a mailbox's opens
     * the join with it. A text that is no code returns why, in one
     * sentence for the screen, and asks nothing; null otherwise.
     */
    static String take(final Activity activity, String text, final Runnable then) {
        final String trimmed = text.trim();
        final Map<String, Object> code = Core.code(trimmed);
        if (code.containsKey("error")) {
            return Core.str(code, "error");
        }
        if ("mailbox".equals(Core.str(code, "kind"))) {
            activity.startActivity(new Intent(activity, MainActivity.class)
                    .putExtra(MainActivity.JOIN, true)
                    .putExtra(MainActivity.CODE, Core.str(code, "lxma")));
            if (then != null) {
                then.run();
            }
            return null;
        }
        // The name the session knows for it, asked on the engine's thread
        // (1464): the read above asks no session.
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.lxma(trimmed);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> known) {
                if (activity.isFinishing() || activity.isDestroyed()) {
                    return;
                }
                ask(activity, known.containsKey("error") ? code : known, then);
            }
        });
        return null;
    }

    /** Ask for the name of the friend `code` read as, add them and open their chat. */
    private static void ask(final Activity activity, Map<String, Object> code, final Runnable then) {
        final String address = Core.str(code, "address");
        final String lxma = Core.str(code, "lxma");
        final EditText name = new EditText(activity);
        name.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_WORDS);
        name.setSingleLine(true);
        name.setText(Core.str(code, "known"));
        name.setSelection(name.getText().length());
        FrameLayout padded = new FrameLayout(activity);
        int pad = Math.round(24 * activity.getResources().getDisplayMetrics().density);
        padded.setPadding(pad, 0, pad, 0);
        padded.addView(name);
        new AlertDialog.Builder(activity)
                .setTitle(R.string.add_contact)
                .setMessage(R.string.add_contact_name)
                .setView(padded)
                .setPositiveButton(R.string.add_contact, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        final String typed = name.getText().toString().trim();
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return Core.addContact(lxma, typed);
                            }
                        }, new Engine.Said() {
                            @Override
                            public void said(String why) {
                                if (why != null) {
                                    Toast.makeText(activity, why, Toast.LENGTH_LONG).show();
                                    return;
                                }
                                activity.startActivity(new Intent(activity, ChatActivity.class)
                                        .putExtra(ChatActivity.PEER, address));
                                if (then != null) {
                                    then.run();
                                }
                            }
                        });
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }
}
