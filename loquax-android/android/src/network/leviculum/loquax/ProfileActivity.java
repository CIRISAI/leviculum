package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.text.InputType;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;
import java.util.Map;

/**
 * Our own screen behind the avatar at the top left of the list (1187); since 1443 the renderer of
 * the core's profile scene (`loquax::ui::profile`), the one the window and the terminal draw: the
 * words are the scene's ({@code words} of the answer, from the catalogue), and the activity comes
 * and goes with the avatar's intent ({@link Core#profileShown}). What stays here is the phone's
 * own: the name's field, the share sheet, the scanner, the code sheets and Advanced.
 *
 * As first built (1187),
 * after the modern messenger cut and Columba's "Your Identity": what a
 * user who knows nothing of Reticulum needs on the main path, the rest
 * under Advanced (`feedback_end_user_apps_whatsapp_simple`).
 *
 * Top to bottom: the avatar large; the name the announce carries, edited
 * in place where the mailbox is on this phone; the address in a
 * monospaced, selectable row with Copy; the QR code of the contact code
 * `lxma://<address>:<key>` that Columba and Sideband read, on a white
 * card; Share (the code as text), Share as picture; Scan a
 * contact's code, which adds a contact from an `lxma://` code or an
 * address alone and joins a mailbox from an `lqxh1` one (loquax::code); where the mailbox is, with the way to
 * another, and where it is on this phone, Link a device (1264), which shows the mailbox's join code
 * for another device and says when that device came, and Move to a new phone (1281), whose code lets
 * the new phone take the mailbox over; Advanced, collapsed: identity hash, home node, propagation
 * node, backup, app version.
 */
public class ProfileActivity extends Activity {
    private static final int SCAN = 1;

    private LinearLayout body;
    private TextView title;
    private Map<String, Object> profile;
    /** The core's profile scene in the reader's words (1443). */
    private Map<String, Object> words;
    private boolean advancedOpen;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout screen = new LinearLayout(this);
        screen.setOrientation(LinearLayout.VERTICAL);
        screen.setFitsSystemWindows(true);
        screen.setBackgroundColor(getColor(R.color.background));
        LinearLayout bar = new LinearLayout(this);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(4), 0, dp(4), 0);
        ImageButton back = new ImageButton(this);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, getColor(R.color.text)));
        back.setBackgroundResource(android.R.color.transparent);
        back.setContentDescription(getString(R.string.back));
        back.setPadding(dp(12), dp(12), dp(12), dp(12));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        bar.addView(back, new LinearLayout.LayoutParams(dp(48), dp(48)));
        title = text(getString(R.string.profile_title), 20, R.color.text);
        title.setId(R.id.profile_title);
        title.setPadding(dp(12), 0, 0, 0);
        bar.addView(title);
        screen.addView(bar, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(64)));
        ScrollView scroll = new ScrollView(this);
        body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(0, 0, 0, dp(24));
        scroll.addView(body);
        screen.addView(scroll, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));
        setContentView(screen);
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
        // The profile's state on the engine's thread (1446); the screen after it.
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.profileShown(true);
            }
        }, new Engine.Said() {
            @Override
            public void said(String error) {
                refresh();
            }
        });
    }

    @Override
    protected void onPause() {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.profileShown(false);
            }
        });
        super.onPause();
    }

    /** The profile again, asked on the engine's thread (1464). */
    private void refresh() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.profile();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (!isDestroyed()) {
                    show(answer);
                }
            }
        });
    }

    private void show(Map<String, Object> answer) {
        body.removeAllViews();
        profile = answer;
        if (profile.containsKey("error")) {
            body.addView(padded(text(Core.str(profile, "error"), 15, R.color.text_secondary)));
            return;
        }
        words = Core.sub(profile, "words");
        title.setText(Core.str(words, "title"));
        top();
        qr();
        share();
        scan();
        mailbox();
        advanced();
    }

    /** The avatar, the name, the address with Copy. */
    private void top() {
        final String address = Core.str(profile, "address");
        AvatarView avatar = new AvatarView(this, null);
        avatar.setId(R.id.profile_avatar);
        avatar.show(address, profile);
        LinearLayout.LayoutParams big = new LinearLayout.LayoutParams(dp(96), dp(96));
        big.gravity = Gravity.CENTER_HORIZONTAL;
        big.topMargin = dp(8);
        body.addView(avatar, big);

        String name = Core.str(profile, "name");
        if (Core.bool(profile, "name_editable")) {
            final EditText edit = new EditText(this);
            edit.setId(R.id.profile_name);
            edit.setText(name);
            edit.setHint(R.string.profile_name_hint);
            edit.setTextSize(22);
            edit.setTextColor(getColor(R.color.text));
            edit.setGravity(Gravity.CENTER);
            edit.setSingleLine(true);
            edit.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_WORDS);
            edit.setImeOptions(EditorInfo.IME_ACTION_DONE);
            edit.setOnEditorActionListener(new TextView.OnEditorActionListener() {
                @Override
                public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                    if (action == EditorInfo.IME_ACTION_DONE) {
                        rename(edit.getText().toString().trim());
                        edit.clearFocus();
                    }
                    return false;
                }
            });
            LinearLayout.LayoutParams wide = matchWrap();
            wide.leftMargin = dp(32);
            wide.rightMargin = dp(32);
            body.addView(edit, wide);
        } else {
            TextView shown = text(Core.str(words, "name"),
                    name.isEmpty() ? 14 : 22, name.isEmpty() ? R.color.text_secondary : R.color.text);
            shown.setId(R.id.profile_name);
            shown.setGravity(Gravity.CENTER);
            shown.setPadding(dp(32), dp(8), dp(32), dp(4));
            body.addView(shown, matchWrap());
        }

        TextView label = text(Core.str(words, "address_label"), 13, R.color.text_secondary);
        label.setPadding(dp(16), dp(16), dp(16), 0);
        body.addView(label);
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(4), dp(8), dp(4));
        TextView hex = text(address, 15, R.color.text);
        hex.setId(R.id.profile_address);
        hex.setTypeface(Typeface.MONOSPACE);
        hex.setTextIsSelectable(true);
        row.addView(hex, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        Button copy = new Button(this);
        copy.setId(R.id.profile_copy);
        copy.setText(R.string.action_copy);
        copy.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                copyAddress();
            }
        });
        row.addView(copy);
        body.addView(row, matchWrap());
        // The address once, with one Copy (1629, walk 3 new 9: it stood
        // twice, whole and in fours, with two buttons that copied it).
    }

    /** The contact code's QR on a white card, and what it is for. */
    private void qr() {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setGravity(Gravity.CENTER_HORIZONTAL);
        card.setPadding(dp(16), dp(16), dp(16), dp(16));
        GradientDrawable white = new GradientDrawable();
        white.setColor(Color.WHITE);
        white.setCornerRadius(dp(16));
        card.setBackground(white);
        ImageView code = QrCode.view(this, profile, dp(240), Core.str(words, "caption"));
        if (code != null) {
            code.setId(R.id.profile_qr);
            card.addView(code, new LinearLayout.LayoutParams(dp(240), dp(240)));
        }
        LinearLayout.LayoutParams centred = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        centred.gravity = Gravity.CENTER_HORIZONTAL;
        centred.topMargin = dp(16);
        body.addView(card, centred);
        TextView caption = text(Core.str(words, "caption"), 14, R.color.text_secondary);
        caption.setGravity(Gravity.CENTER);
        caption.setPadding(dp(16), dp(8), dp(16), dp(8));
        body.addView(caption, matchWrap());
    }

    /** Share the code as text and as a picture; the address row above copies it. */
    private void share() {
        // "Share my address" (1443): the scene's one share, the phone's share sheet.
        Button text = wide(Core.str(words, "share"), R.id.profile_share);
        text.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Intent send = new Intent(Intent.ACTION_SEND)
                        .setType("text/plain")
                        .putExtra(Intent.EXTRA_TEXT, Core.str(profile, "lxma"));
                startActivity(Intent.createChooser(send, getString(R.string.share)));
            }
        });
        Button picture = wide(getString(R.string.profile_share_picture), R.id.profile_share_picture);
        picture.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                sharePicture();
            }
        });
    }

    /** The scanner, for a contact's code or a mailbox's. */
    private void scan() {
        if (!ScanActivity.available(this)) {
            return;
        }
        TextView scan = row(getString(R.string.profile_scan), R.id.profile_scan);
        // An action looks like one (1632, walk 4 new 19: plain text under
        // two real buttons), as "Standort einschalten" does since 1629.
        scan.setTextColor(getColor(R.color.accent));
        scan.setTypeface(null, android.graphics.Typeface.BOLD);
        scan.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivityForResult(new Intent(ProfileActivity.this, ScanActivity.class)
                        .putExtra(ScanActivity.PREFIX, ScanActivity.CODES), SCAN);
            }
        });
    }

    /**
     * Where the mailbox is and how it is reached, the devices of this identity (1443, the scene's
     * words), and the way to another mailbox.
     */
    private void mailbox() {
        TextView said = text(Core.str(words, "mailbox"), 15, R.color.text);
        said.setId(R.id.profile_mailbox);
        said.setPadding(dp(16), dp(24), dp(16), dp(4));
        body.addView(said, matchWrap());
        String state = Core.str(words, "state");
        if (!state.isEmpty()) {
            TextView how = text(state, 14, R.color.text_secondary);
            how.setId(R.id.profile_state);
            how.setPadding(dp(16), 0, dp(16), dp(4));
            body.addView(how, matchWrap());
        }
        java.util.List<String> devices = Core.words(words, "devices");
        if (!devices.isEmpty()) {
            TextView label = text(Core.str(words, "devices_label"), 13, R.color.text_secondary);
            label.setPadding(dp(16), dp(16), dp(16), 0);
            body.addView(label);
            StringBuilder lines = new StringBuilder();
            for (String device : devices) {
                lines.append(lines.length() == 0 ? "" : "\n").append(device);
            }
            TextView list = text(lines.toString(), 15, R.color.text);
            list.setId(R.id.profile_devices);
            list.setPadding(dp(16), dp(2), dp(16), dp(4));
            body.addView(list, matchWrap());
        }
        if (Core.bool(profile, "here")) {
            TextView link = row(getString(R.string.profile_link_device), R.id.profile_link_device);
            link.setTextColor(getColor(R.color.accent));
            link.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    linkDevice();
                }
            });
            TextView move = row(getString(R.string.profile_move_phone), R.id.profile_move_phone);
            move.setTextColor(getColor(R.color.accent));
            move.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    movePhone();
                }
            });
        }
        TextView change = row(getString(R.string.profile_mailbox_change), R.id.profile_mailbox_change);
        change.setTextColor(getColor(R.color.accent));
        change.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(ProfileActivity.this, MainActivity.class)
                        .putExtra(MainActivity.JOIN, true));
            }
        });
    }

    /**
     * Link a device (1264): the mailbox's join code for one other device, and a line that asks the
     * library every second whether the device came: "Device linked: <its name>" then, or that the
     * code ran out with a new one a tap away.
     */
    private void linkDevice() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.linkDevice();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> code) {
                if (!isDestroyed()) {
                    linkDevice(code);
                }
            }
        });
    }

    /** {@link #linkDevice}'s sheet for the library's `code`. */
    private void linkDevice(Map<String, Object> code) {
        codeSheet(code, R.string.link_title, R.string.link_text, R.string.link_waiting,
                new int[] {R.id.link_text, R.id.link_qr, R.id.link_code, R.id.link_status},
                new Look() {
                    @Override
                    public Map<String, Object> ask() {
                        return Core.linked();
                    }

                    @Override
                    public boolean look(TextView status, Map<String, Object> linked) {
                        String state = Core.str(linked, "state");
                        if ("linked".equals(state)) {
                            status.setText(getString(R.string.link_linked, Core.str(linked, "name")));
                            status.setTextColor(getColor(R.color.accent));
                            return true;
                        }
                        if ("over".equals(state)) {
                            status.setText(R.string.link_over);
                            return true;
                        }
                        return false;
                    }
                }, false);
    }

    /**
     * Move to a new phone (1281, walk finding 24): a code for the new phone, which takes the address,
     * the contacts and the messages over it; the line says when it did, and this phone has stopped
     * then. Done after the move opens the first screen, which says this phone moved.
     */
    private void movePhone() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.movePhone();
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> code) {
                if (!isDestroyed()) {
                    movePhone(code);
                }
            }
        });
    }

    /** {@link #movePhone}'s sheet for the library's `code`. */
    private void movePhone(Map<String, Object> code) {
        codeSheet(code, R.string.move_title, R.string.move_text, R.string.move_waiting,
                new int[] {R.id.link_text, R.id.move_qr, R.id.move_code, R.id.move_status},
                new Look() {
                    @Override
                    public Map<String, Object> ask() {
                        return Core.moving();
                    }

                    @Override
                    public boolean look(TextView status, Map<String, Object> moving) {
                        String state = Core.str(moving, "state");
                        if ("moved".equals(state)) {
                            status.setText(getString(R.string.move_moved, Core.str(moving, "name")));
                            status.setTextColor(getColor(R.color.accent));
                            return true;
                        }
                        if ("over".equals(state)) {
                            status.setText(R.string.link_over);
                            return true;
                        }
                        return false;
                    }
                }, true);
    }

    /**
     * What a code sheet asks the library every second, on the engine's
     * thread (1464), and its answer shown; true once it is final.
     */
    private interface Look extends Engine.Ask<Map<String, Object>> {
        boolean look(TextView status, Map<String, Object> answer);
    }

    /**
     * A code for another device (1264, 1281): the instruction, the code as a QR on a white card and
     * as text to share, how long it is good for, and the status line `look` keeps. `ids` are the
     * instruction's, the QR's, the text's and the status line's. With `restart`, Done after a final
     * answer opens the first screen afresh.
     */
    private void codeSheet(final Map<String, Object> code, int title, int how, int waiting, int[] ids,
            final Look look, final boolean restart) {
        if (code.containsKey("error")) {
            Toast.makeText(this, Core.str(code, "error"), Toast.LENGTH_LONG).show();
            return;
        }
        LinearLayout sheet = new LinearLayout(this);
        sheet.setOrientation(LinearLayout.VERTICAL);
        sheet.setGravity(Gravity.CENTER_HORIZONTAL);
        sheet.setPadding(dp(16), dp(16), dp(16), dp(8));
        TextView said = text(getString(how), 15, R.color.text);
        said.setId(ids[0]);
        sheet.addView(said, matchWrap());
        LinearLayout card = new LinearLayout(this);
        card.setGravity(Gravity.CENTER);
        card.setPadding(dp(12), dp(12), dp(12), dp(12));
        GradientDrawable white = new GradientDrawable();
        white.setColor(Color.WHITE);
        white.setCornerRadius(dp(16));
        card.setBackground(white);
        ImageView qr = QrCode.view(this, code, dp(240), getString(title));
        if (qr != null) {
            qr.setId(ids[1]);
            card.addView(qr, new LinearLayout.LayoutParams(dp(240), dp(240)));
        }
        LinearLayout.LayoutParams centred = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        centred.gravity = Gravity.CENTER_HORIZONTAL;
        centred.topMargin = dp(12);
        sheet.addView(card, centred);
        TextView good = text(getString(R.string.link_good_for, (int) Core.num(code, "hours")), 14,
                R.color.text_secondary);
        good.setGravity(Gravity.CENTER);
        good.setPadding(0, dp(8), 0, 0);
        sheet.addView(good, matchWrap());
        // The code as text, for a device that pastes rather than scans; the walk reads it here.
        TextView text = text(Core.str(code, "code"), 11, R.color.text_secondary);
        text.setId(ids[2]);
        text.setTypeface(Typeface.MONOSPACE);
        text.setTextIsSelectable(true);
        text.setPadding(0, dp(8), 0, 0);
        sheet.addView(text, matchWrap());
        final TextView status = text(getString(waiting), 16, R.color.text);
        status.setId(ids[3]);
        status.setGravity(Gravity.CENTER);
        status.setPadding(0, dp(12), 0, 0);
        sheet.addView(status, matchWrap());
        ScrollView scroll = new ScrollView(this);
        scroll.addView(sheet);
        final boolean[] over = new boolean[1];
        final AlertDialog dialog = new AlertDialog.Builder(this)
                .setTitle(title)
                .setView(scroll)
                .setNegativeButton(R.string.link_share, null)
                .setPositiveButton(R.string.done, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface shown, int which) {
                        if (restart && over[0]) {
                            startActivity(new Intent(ProfileActivity.this, MainActivity.class)
                                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
                        }
                    }
                })
                .create();
        final Runnable[] poll = new Runnable[1];
        poll[0] = new Runnable() {
            @Override
            public void run() {
                if (!dialog.isShowing()) {
                    return;
                }
                Engine.ask(look, new Engine.Got<Map<String, Object>>() {
                    @Override
                    public void got(Map<String, Object> answer) {
                        if (!dialog.isShowing()) {
                            return;
                        }
                        if (look.look(status, answer)) {
                            over[0] = true;
                            return;
                        }
                        status.postDelayed(poll[0], 1000);
                    }
                });
            }
        };
        dialog.setOnShowListener(new DialogInterface.OnShowListener() {
            @Override
            public void onShow(DialogInterface shown) {
                dialog.getButton(AlertDialog.BUTTON_NEGATIVE).setOnClickListener(
                        new View.OnClickListener() {
                            @Override
                            public void onClick(View v) {
                                Intent send = new Intent(Intent.ACTION_SEND)
                                        .setType("text/plain")
                                        .putExtra(Intent.EXTRA_TEXT, Core.str(code, "code"));
                                startActivity(Intent.createChooser(send, getString(R.string.link_share)));
                            }
                        });
                status.postDelayed(poll[0], 1000);
            }
        });
        dialog.show();
    }

    /** Advanced, collapsed: what needs Reticulum knowledge. */
    private void advanced() {
        final TextView header = row(getString(R.string.advanced) + (advancedOpen ? "  ▴" : "  ▾"),
                R.id.profile_advanced);
        header.setPadding(dp(16), dp(24), dp(16), dp(12));
        final LinearLayout facts = new LinearLayout(this);
        facts.setId(R.id.profile_advanced_body);
        facts.setOrientation(LinearLayout.VERTICAL);
        facts.setVisibility(advancedOpen ? View.VISIBLE : View.GONE);
        body.addView(facts, matchWrap());
        header.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                advancedOpen = !advancedOpen;
                facts.setVisibility(advancedOpen ? View.VISIBLE : View.GONE);
                header.setText(getString(R.string.advanced) + (advancedOpen ? "  ▴" : "  ▾"));
            }
        });
        fact(facts, getString(R.string.profile_identity), Core.str(profile, "identity"), true);
        fact(facts, getString(R.string.profile_home_node), Core.str(profile, "home_node"), true);
        String rendezvous = Core.str(profile, "rendezvous");
        if (!rendezvous.isEmpty()) {
            fact(facts, getString(R.string.profile_rendezvous), rendezvous, true);
        }
        String node = Core.str(profile, "propagation_node");
        fact(facts, getString(R.string.propagation_node),
                node.isEmpty() ? getString(R.string.profile_pn_chosen) : node, !node.isEmpty());
        if (Core.bool(profile, "here")) {
            TextView backup = text(getString(R.string.backup_title), 16, R.color.text);
            backup.setPadding(dp(16), dp(12), dp(16), dp(12));
            backup.setBackgroundResource(android.R.drawable.list_selector_background);
            backup.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startActivity(new Intent(ProfileActivity.this, BackupActivity.class));
                }
            });
            facts.addView(backup, matchWrap());
        } else {
            fact(facts, getString(R.string.settings_backup), getString(R.string.settings_backup_away), false);
        }
        fact(facts, getString(R.string.profile_version), Core.str(profile, "version"), false);
    }

    /** One fact of Advanced: its label, and its value, monospaced and selectable when a hash. */
    private void fact(LinearLayout into, String label, String value, boolean hash) {
        TextView name = text(label, 13, R.color.text_secondary);
        name.setPadding(dp(16), dp(8), dp(16), 0);
        into.addView(name);
        TextView shown = text(value, hash ? 14 : 15, R.color.text);
        if (hash) {
            shown.setTypeface(Typeface.MONOSPACE);
            shown.setTextIsSelectable(true);
        }
        shown.setPadding(dp(16), dp(2), dp(16), dp(4));
        into.addView(shown, matchWrap());
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        if (request != SCAN || result != RESULT_OK || data == null) {
            return;
        }
        // A friend's code adds them, a mailbox's joins it (1263).
        String why = AddContact.take(this, String.valueOf(data.getStringExtra(ScanActivity.TEXT)), null);
        if (why != null) {
            Toast.makeText(this, why, Toast.LENGTH_LONG).show();
        }
    }

    /** The name the announce carries, kept off the UI thread: the mailbox restarts with it. */
    private void rename(final String name) {
        new Thread(new Runnable() {
            @Override
            public void run() {
                final String why = Core.name(name);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        Toast.makeText(ProfileActivity.this,
                                why == null ? getString(R.string.profile_name_saved) : why,
                                Toast.LENGTH_SHORT).show();
                    }
                });
            }
        }, "loquax-name").start();
    }

    private void copyAddress() {
        ClipboardManager clipboard = getSystemService(ClipboardManager.class);
        if (clipboard != null) {
            clipboard.setPrimaryClip(ClipData.newPlainText("LXMF", Core.str(profile, "address")));
            Toast.makeText(this, R.string.address_copied, Toast.LENGTH_SHORT).show();
        }
    }

    /** The QR code as a PNG to the app the user picks, granted read access to it alone. */
    private void sharePicture() {
        Bitmap bitmap = QrCode.bitmap(profile.get("qr"), (int) Core.num(profile, "qr_size"), 1024);
        Uri uri = bitmap == null ? null : QrProvider.write(this, bitmap);
        if (uri == null) {
            return;
        }
        Intent send = new Intent(Intent.ACTION_SEND)
                .setType("image/png")
                .putExtra(Intent.EXTRA_STREAM, uri)
                .putExtra(Intent.EXTRA_TEXT, Core.str(profile, "lxma"))
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        send.setClipData(ClipData.newRawUri("", uri));
        startActivity(Intent.createChooser(send, getString(R.string.share)));
    }

    private Button wide(String words, int id) {
        Button button = new Button(this);
        button.setId(id);
        button.setText(words);
        LinearLayout.LayoutParams params = matchWrap();
        params.leftMargin = dp(16);
        params.rightMargin = dp(16);
        params.topMargin = dp(8);
        body.addView(button, params);
        return button;
    }

    private TextView row(String words, int id) {
        TextView row = text(words, 16, R.color.text);
        row.setId(id);
        row.setPadding(dp(16), dp(14), dp(16), dp(14));
        row.setBackgroundResource(android.R.drawable.list_selector_background);
        body.addView(row, matchWrap());
        return row;
    }

    private View padded(TextView view) {
        view.setPadding(dp(16), dp(8), dp(16), dp(8));
        return view;
    }

    private TextView text(String words, float sp, int colour) {
        TextView view = new TextView(this);
        view.setText(words);
        view.setTextSize(sp);
        view.setTextColor(getColor(colour));
        return view;
    }

    private LinearLayout.LayoutParams matchWrap() {
        return new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}
