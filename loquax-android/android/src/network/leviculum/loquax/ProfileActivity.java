package network.leviculum.loquax;

import android.app.Activity;
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
import android.text.format.DateUtils;
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
 * Our own screen behind the avatar at the top left of the list (1187),
 * after the modern messenger cut and Columba's "Your Identity": what a
 * user who knows nothing of Reticulum needs on the main path, the rest
 * under Advanced (`feedback_end_user_apps_whatsapp_simple`).
 *
 * Top to bottom: the avatar large; the name the announce carries, edited
 * in place where the mailbox is on this phone; the address in a
 * monospaced, selectable row with Copy; the QR code of the contact code
 * `lxma://<address>:<key>` that Columba and Sideband read, on a white
 * card; Share (the code as text), Share as picture, Copy address; Scan a
 * contact's code, which adds a contact from an `lxma://` code and joins a
 * mailbox from an `lqxh1` one; where the mailbox is, with the way to
 * another; Advanced, collapsed: identity hash, home node, propagation
 * node, backup, app version.
 */
public class ProfileActivity extends Activity {
    private static final int SCAN = 1;
    /** What the scanner takes here: a contact's code or a mailbox's. */
    static final String SCAN_PREFIXES = "lxma://|lqxh1";

    private LinearLayout body;
    private Map<String, Object> profile;
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
        TextView title = text(getString(R.string.profile_title), 20, R.color.text);
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
        refresh();
    }

    private void refresh() {
        body.removeAllViews();
        profile = Core.profile();
        if (profile.containsKey("error")) {
            body.addView(padded(text(Core.str(profile, "error"), 15, R.color.text_secondary)));
            return;
        }
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
            TextView shown = text(name.isEmpty() ? getString(R.string.profile_name_elsewhere) : name,
                    name.isEmpty() ? 14 : 22, name.isEmpty() ? R.color.text_secondary : R.color.text);
            shown.setId(R.id.profile_name);
            shown.setGravity(Gravity.CENTER);
            shown.setPadding(dp(32), dp(8), dp(32), dp(4));
            body.addView(shown, matchWrap());
        }

        TextView label = text(getString(R.string.profile_address), 13, R.color.text_secondary);
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
        ImageView code = QrCode.view(this, profile, dp(240), getString(R.string.profile_qr_caption));
        if (code != null) {
            code.setId(R.id.profile_qr);
            card.addView(code, new LinearLayout.LayoutParams(dp(240), dp(240)));
        }
        LinearLayout.LayoutParams centred = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        centred.gravity = Gravity.CENTER_HORIZONTAL;
        centred.topMargin = dp(16);
        body.addView(card, centred);
        TextView caption = text(getString(R.string.profile_qr_caption), 14, R.color.text_secondary);
        caption.setGravity(Gravity.CENTER);
        caption.setPadding(dp(16), dp(8), dp(16), dp(8));
        body.addView(caption, matchWrap());
    }

    /** Share the code as text, as a picture, and copy the address. */
    private void share() {
        Button text = wide(getString(R.string.share), R.id.profile_share);
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
        TextView copy = row(getString(R.string.profile_copy_address), R.id.profile_copy_address);
        copy.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                copyAddress();
            }
        });
    }

    /** The scanner, for a contact's code or a mailbox's. */
    private void scan() {
        if (!ScanActivity.available(this)) {
            return;
        }
        TextView scan = row(getString(R.string.profile_scan), R.id.profile_scan);
        scan.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivityForResult(new Intent(ProfileActivity.this, ScanActivity.class)
                        .putExtra(ScanActivity.PREFIX, SCAN_PREFIXES), SCAN);
            }
        });
    }

    /** Where the mailbox is, in one line, and the way to another. */
    private void mailbox() {
        String line;
        if (Core.bool(profile, "here")) {
            line = getString(R.string.profile_mailbox_here);
        } else {
            long since = Core.num(profile, "since");
            String where = Core.str(profile, "mailbox");
            line = since > 0
                    ? getString(R.string.profile_mailbox_away, where, DateUtils.formatDateTime(
                            this, since * 1000, DateUtils.FORMAT_SHOW_DATE | DateUtils.FORMAT_SHOW_YEAR))
                    : getString(R.string.profile_mailbox_away_undated, where);
        }
        TextView said = text(line, 15, R.color.text);
        said.setId(R.id.profile_mailbox);
        said.setPadding(dp(16), dp(24), dp(16), dp(4));
        body.addView(said, matchWrap());
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
        String text = String.valueOf(data.getStringExtra(ScanActivity.TEXT));
        if (AddContact.is(text)) {
            AddContact.ask(this, text, null);
        } else {
            // A mailbox's code: the join the first screen does for one.
            startActivity(new Intent(this, MainActivity.class)
                    .putExtra(MainActivity.JOIN, true)
                    .putExtra(MainActivity.CODE, text));
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
