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
 * A contact from its contact code, `lxma://<address>:<key>` (1187), the
 * code Columba and Sideband share: scanned on the profile or pasted into
 * the pencil's field. The library says whose it is; the user names it;
 * the library keeps the name, hands the key to the mailbox and opens the
 * conversation, which this opens on screen.
 */
final class AddContact {
    private static final String PREFIX = "lxma://";

    private AddContact() {}

    /** Whether `text` is meant as a contact code. */
    static boolean is(String text) {
        return text != null && text.trim().regionMatches(true, 0, PREFIX, 0, PREFIX.length());
    }

    /**
     * Ask for the name of the contact `text` is the code of, add it and
     * open its chat, then run `then` (null for nothing). A code that is
     * none says why and asks nothing.
     */
    static void ask(final Activity activity, String text, final Runnable then) {
        Map<String, Object> code = Core.lxma(text.trim());
        if (code.containsKey("error")) {
            Toast.makeText(activity, Core.str(code, "error"), Toast.LENGTH_LONG).show();
            return;
        }
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
                        String why = Core.addContact(lxma, name.getText().toString().trim());
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
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }
}
