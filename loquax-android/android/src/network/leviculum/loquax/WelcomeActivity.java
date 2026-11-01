package network.leviculum.loquax;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.Settings;
import android.text.InputType;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.RadioButton;
import android.widget.RadioGroup;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.File;
import java.util.Map;

/**
 * The first start (1152; docs/drafts/loquax-onboarding-2026-10.md,
 * section 2), one question per screen and the recommended answer
 * preselected:
 *
 * 1. welcome and "Where should your mailbox be?" on one screen, "On this
 *    phone" chosen, its three warnings below it while it is;
 * 2. for a computer: what to do there, then the code (scan it or paste
 *    it), or "use this phone until then";
 * 3. the background permission: notifications and the battery exemption,
 *    explained once, then Android's two dialogs;
 * 4. "Your address": the QR code, share, write a message, the optional
 *    name, and for a mailbox on the phone the backup.
 *
 * Beside the choice, "I already have a Loquax address": a backup line,
 * from its file, scanned off the paper or typed, and its passphrase, then the
 * phone's mailbox with that address.
 *
 * Shown while the phone neither holds a mailbox nor reads one; the first
 * screen sends it here. A code pasted for a computer's mailbox goes to the
 * join form of the first screen, which joins as before.
 */
public class WelcomeActivity extends Activity {
    private static final String PAGE = "page";
    private static final String WHERE_HERE = "here";
    private static final int CHOICE = 0;
    private static final int COMPUTER = 1;
    private static final int BACKGROUND = 2;
    private static final int STARTING = 3;
    private static final int ADDRESS = 4;
    private static final int RESTORE = 5;
    /** Another device's mailbox (1264): a phone's "Link a device" code, or a computer's. */
    private static final int JOINING = 6;
    private static final int ASK_NOTIFICATIONS = 1;
    private static final int ASK_BATTERY = 2;
    private static final int OPEN_BACKUP = 3;
    private static final int SCAN_CODE = 4;
    private static final int SCAN_BACKUP = 5;
    /** The extra that opens a page directly; {@link #ADDRESS_PAGE} is the one there is. */
    static final String SHOW = "show";
    static final String ADDRESS_PAGE = "address";

    private ScrollView frame;
    private int page = CHOICE;
    private boolean here = true;
    private Map<String, Object> me = Core.map();
    private String failed;
    private EditText name;
    /** A backup line to restore from, and its passphrase; null for a new address. */
    private String restore;
    private String passphrase;
    private EditText backupLine;

    /** Whether the phone holds a mailbox or reads one: then the first start is over. */
    static boolean settled(Activity screen) {
        File dir = screen.getFilesDir();
        return new File(dir, "mailbox-here").exists() || new File(dir, "home-client").exists();
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        frame = new ScrollView(this);
        frame.setFitsSystemWindows(true);
        frame.setBackgroundColor(getColor(R.color.background));
        setContentView(frame);
        if (savedInstanceState != null) {
            page = savedInstanceState.getInt(PAGE, CHOICE);
            here = savedInstanceState.getBoolean(WHERE_HERE, true);
        } else if (ADDRESS_PAGE.equals(getIntent().getStringExtra(SHOW))) {
            // The address page of a session that runs: after the first
            // join of a mailbox elsewhere (walk finding 13), and the device
            // walk's way to the first start's QR (1263), in the debug APK,
            // where this screen is exported.
            page = ADDRESS;
        }
        // Recreated while starting or after: the session runs or ran in
        // the library; asking it again answers for the running one.
        if (page == STARTING || page == ADDRESS) {
            start();
        } else {
            show(page);
        }
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putInt(PAGE, page);
        state.putBoolean(WHERE_HERE, here);
    }

    @Override
    public void onBackPressed() {
        if (page == RESTORE) {
            failed = null;
            show(CHOICE);
        } else if (page == JOINING) {
            show(CHOICE);
        } else if (page == COMPUTER || page == BACKGROUND) {
            show(page == BACKGROUND && !here ? COMPUTER : CHOICE);
        } else {
            super.onBackPressed();
        }
    }

    private void show(int which) {
        page = which;
        LinearLayout body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(dp(24), dp(32), dp(24), dp(24));
        switch (which) {
            case CHOICE:
                choice(body);
                break;
            case COMPUTER:
                computer(body, false);
                break;
            case JOINING:
                computer(body, true);
                break;
            case BACKGROUND:
                background(body);
                break;
            case STARTING:
                starting(body);
                break;
            case RESTORE:
                restoring(body);
                break;
            default:
                address(body);
                break;
        }
        frame.removeAllViews();
        frame.addView(body, new ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
    }

    /** Welcome, and where the mailbox lives; the phone's warnings while it is chosen. */
    private void choice(LinearLayout body) {
        body.addView(text(getString(R.string.app_name), 30, R.color.text, true), matchWrap());
        body.addView(gap(text(getString(R.string.welcome_tagline), 16, R.color.text_secondary,
                false), 4, 24), matchWrap());
        body.addView(text(getString(R.string.where_title), 22, R.color.text, true), matchWrap());
        body.addView(gap(text(getString(R.string.where_text), 15, R.color.text, false), 8, 12),
                matchWrap());
        final RadioGroup group = new RadioGroup(this);
        final RadioButton phone = option(getString(R.string.where_phone));
        final RadioButton computer = option(getString(R.string.where_computer));
        group.addView(phone);
        group.addView(indented(getString(R.string.where_phone_text)));
        group.addView(computer);
        group.addView(indented(getString(R.string.where_computer_text)));
        body.addView(group, matchWrap());
        final LinearLayout warnings = new LinearLayout(this);
        warnings.setOrientation(LinearLayout.VERTICAL);
        warnings.setPadding(dp(8), dp(12), 0, 0);
        warnings.addView(text(getString(R.string.here_title), 17, R.color.text, true), matchWrap());
        for (int line : new int[] {R.string.here_background, R.string.here_offline,
                R.string.here_backup}) {
            warnings.addView(gap(text("• " + getString(line), 15, R.color.text, false), 6, 0),
                    matchWrap());
        }
        body.addView(warnings, matchWrap());
        body.addView(gap(text(getString(R.string.where_later), 14, R.color.text_secondary,
                false), 16, 8), matchWrap());
        final Button next = button(getString(here ? R.string.use_this_phone : R.string.next));
        body.addView(next, wrap());
        group.setOnCheckedChangeListener(new RadioGroup.OnCheckedChangeListener() {
            @Override
            public void onCheckedChanged(RadioGroup g, int checked) {
                here = checked == phone.getId();
                warnings.setVisibility(here ? View.VISIBLE : View.GONE);
                next.setText(here ? R.string.use_this_phone : R.string.next);
            }
        });
        group.check(here ? phone.getId() : computer.getId());
        next.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                restore = null;
                show(here ? BACKGROUND : COMPUTER);
            }
        });
        Button join = button(getString(R.string.join_existing));
        join.setId(R.id.where_join);
        join.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                here = false;
                restore = null;
                show(JOINING);
            }
        });
        body.addView(join, wrap());
        Button have = button(getString(R.string.have_address));
        have.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                show(RESTORE);
            }
        });
        body.addView(have, wrap());
    }

    /** An address from a backup: its line and its passphrase. */
    private void restoring(LinearLayout body) {
        body.addView(text(getString(R.string.restore_title), 22, R.color.text, true), matchWrap());
        body.addView(gap(text(getString(R.string.restore_text), 15, R.color.text, false), 8, 12),
                matchWrap());
        Button open = button(getString(R.string.restore_file));
        open.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivityForResult(new Intent(Intent.ACTION_OPEN_DOCUMENT)
                        .addCategory(Intent.CATEGORY_OPENABLE)
                        .setType("text/*"), OPEN_BACKUP);
            }
        });
        body.addView(open, wrap());
        if (ScanActivity.available(this)) {
            Button scan = button(getString(R.string.scan_backup));
            scan.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startActivityForResult(new Intent(WelcomeActivity.this, ScanActivity.class)
                            .putExtra(ScanActivity.PREFIX, "lqxb1"), SCAN_BACKUP);
                }
            });
            body.addView(scan, wrap());
        }
        backupLine = new EditText(this);
        backupLine.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE
                | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        backupLine.setHint(R.string.restore_hint);
        if (restore != null) {
            backupLine.setText(restore);
        }
        body.addView(backupLine, matchWrap());
        final EditText secret = new EditText(this);
        secret.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
        secret.setHint(R.string.passphrase);
        body.addView(secret, matchWrap());
        if (failed != null) {
            body.addView(gap(text(failed, 15, R.color.text, false), 8, 0), matchWrap());
        }
        Button next = button(getString(R.string.restore));
        next.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String line = backupLine(backupLine.getText().toString());
                if (line.isEmpty()) {
                    failed = getString(R.string.restore_none);
                    show(RESTORE);
                    return;
                }
                restore = line;
                passphrase = secret.getText().toString();
                failed = null;
                here = true;
                show(BACKGROUND);
            }
        });
        body.addView(next, wrap());
    }

    /** The backup line in `text`, the file's or the typed one, from its prefix on; empty for none. */
    static String backupLine(String text) {
        int at = text.toLowerCase(java.util.Locale.ROOT).indexOf("lqxb1");
        return at < 0 ? "" : text.substring(at).trim();
    }

    /**
     * The mailbox on a computer: what to do there, and the code it shows; `joining`, a mailbox that
     * already runs on another device (1264), whose "Link a device" shows the same code.
     */
    private void computer(LinearLayout body, boolean joining) {
        body.addView(text(getString(joining ? R.string.join_existing : R.string.computer_title), 22,
                R.color.text, true), matchWrap());
        body.addView(gap(text(getString(joining ? R.string.join_existing_text : R.string.computer_text),
                15, R.color.text, false), 8, 16), matchWrap());
        if (ScanActivity.available(this)) {
            Button scan = button(getString(R.string.scan_code));
            scan.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startActivityForResult(new Intent(WelcomeActivity.this, ScanActivity.class)
                            .putExtra(ScanActivity.PREFIX, "lqxh1"), SCAN_CODE);
                }
            });
            body.addView(scan, wrap());
        }
        Button paste = button(getString(R.string.paste_code));
        paste.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(WelcomeActivity.this, MainActivity.class)
                        .putExtra(MainActivity.JOIN, true));
            }
        });
        body.addView(paste, wrap());
        if (joining) {
            return;
        }
        Button until = button(getString(R.string.phone_until_then));
        until.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                here = true;
                show(BACKGROUND);
            }
        });
        body.addView(until, wrap());
    }

    /** Why the app asks to stay in the background, before Android's two dialogs. */
    private void background(LinearLayout body) {
        body.addView(text(getString(R.string.background_title), 22, R.color.text, true),
                matchWrap());
        body.addView(gap(text(getString(R.string.background_text), 15, R.color.text, false), 8,
                16), matchWrap());
        Button next = button(getString(R.string.next));
        next.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Asks.asked(WelcomeActivity.this);
                askNotifications();
            }
        });
        body.addView(next, wrap());
    }

    private void askNotifications() {
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS},
                    ASK_NOTIFICATIONS);
            return;
        }
        askBattery();
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        if (request == ASK_NOTIFICATIONS) {
            // A refusal is accepted: the notification says so later.
            askBattery();
        }
    }

    private void askBattery() {
        PowerManager power = getSystemService(PowerManager.class);
        if (power.isIgnoringBatteryOptimizations(getPackageName())) {
            start();
            return;
        }
        try {
            startActivityForResult(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    Uri.parse("package:" + getPackageName())), ASK_BATTERY);
        } catch (RuntimeException none) {
            // A phone without the dialog: nothing to ask.
            start();
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        if (request == ASK_BATTERY) {
            start();
        } else if (request == OPEN_BACKUP && result == RESULT_OK && data != null
                && data.getData() != null && backupLine != null) {
            backupLine.setText(backupLine(read(data.getData())));
        } else if (request == SCAN_BACKUP && result == RESULT_OK && data != null
                && backupLine != null) {
            backupLine.setText(backupLine(String.valueOf(data.getStringExtra(ScanActivity.TEXT))));
        } else if (request == SCAN_CODE && result == RESULT_OK && data != null) {
            // The join itself is the first screen's, as for a pasted code.
            startActivity(new Intent(this, MainActivity.class)
                    .putExtra(MainActivity.JOIN, true)
                    .putExtra(MainActivity.CODE, data.getStringExtra(ScanActivity.TEXT)));
        }
    }

    /** A small text file the picker opened; empty when it cannot be read. */
    private String read(Uri file) {
        try (java.io.InputStream in = getContentResolver().openInputStream(file)) {
            if (in == null) {
                return "";
            }
            java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
            byte[] chunk = new byte[4096];
            int read;
            while ((read = in.read(chunk)) > 0 && out.size() < 65536) {
                out.write(chunk, 0, read);
            }
            return new String(out.toByteArray(), java.nio.charset.StandardCharsets.UTF_8);
        } catch (java.io.IOException | RuntimeException unreadable) {
            return "";
        }
    }

    /** The session: with the mailbox on this phone, made on the first call. */
    private void start() {
        failed = null;
        show(STARTING);
        final String dir = getFilesDir().getAbsolutePath();
        new Thread(new Runnable() {
            @Override
            public void run() {
                Core.sense(getApplicationContext());
                final Map<String, Object> started = Core.startHere(dir, restore, passphrase);
                final Map<String, Object> mine = Core.bool(started, "ok") ? Core.me() : null;
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        started(started, mine);
                    }
                });
            }
        }, "loquax-first-start").start();
    }

    private void started(Map<String, Object> started, Map<String, Object> mine) {
        if (!Core.bool(started, "ok")) {
            failed = Core.str(started, "error");
            // A backup that did not open: its screen again, the line kept.
            show("passphrase".equals(Core.str(started, "need")) ? RESTORE : STARTING);
            return;
        }
        restore = null;
        passphrase = null;
        Events.start();
        LoquaxService.start(this);
        me = mine;
        show(ADDRESS);
    }

    /** While the mailbox starts, and why it did not. */
    private void starting(LinearLayout body) {
        if (failed == null) {
            body.addView(text(getString(R.string.setting_up), 17, R.color.text, false),
                    matchWrap());
            return;
        }
        body.addView(text(getString(R.string.setup_failed, failed), 15, R.color.text, false),
                matchWrap());
        Button again = button(getString(R.string.try_again));
        again.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                start();
            }
        });
        body.addView(again, wrap());
    }

    /** The name for others, the address to give people, the backup. */
    private void address(LinearLayout body) {
        final boolean phone = Core.bool(me, "here");
        if (phone) {
            // The page's one field, first (walk finding 9): friends see this
            // name, not the address's digits. Prefilled from the phone
            // owner's card where Android lets us read it; empty is allowed.
            body.addView(text(getString(R.string.name_for_others), 15, R.color.text, false),
                    matchWrap());
            name = new EditText(this);
            name.setId(R.id.welcome_name);
            name.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_WORDS);
            name.setSingleLine(true);
            name.setHint(R.string.profile_name_hint);
            String kept = Core.str(me, "name");
            name.setText(kept.isEmpty() ? Book.ownerName(this) : kept);
            body.addView(name, matchWrap());
        }
        body.addView(gap(text(getString(R.string.address_title), 22, R.color.text, true), 16, 0),
                matchWrap());
        // The contact code's QR, the profile's (1263): a friend's scanner
        // takes it.
        ImageView qr = QrCode.view(this, me, dp(240), Core.str(me, "fingerprint"));
        if (qr != null) {
            qr.setId(R.id.welcome_qr);
            LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(dp(240), dp(240));
            at.gravity = Gravity.CENTER_HORIZONTAL;
            at.topMargin = dp(16);
            body.addView(qr, at);
        }
        TextView fingerprint = text(Core.str(me, "fingerprint"), 16, R.color.text, false);
        fingerprint.setTypeface(Typeface.MONOSPACE);
        fingerprint.setGravity(Gravity.CENTER);
        fingerprint.setTextIsSelectable(true);
        body.addView(gap(fingerprint, 12, 4), matchWrap());
        body.addView(gap(text(getString(R.string.address_text), 15, R.color.text_secondary,
                false), 4, 16), matchWrap());
        LinearLayout row = new LinearLayout(this);
        Button share = button(getString(R.string.share));
        share.setId(R.id.welcome_share);
        share.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Intent send = new Intent(Intent.ACTION_SEND)
                        .setType("text/plain")
                        .putExtra(Intent.EXTRA_TEXT, Core.str(me, "lxma"));
                startActivity(Intent.createChooser(send, getString(R.string.share)));
            }
        });
        row.addView(share, wrap());
        Button write = button(getString(R.string.write_message));
        write.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                done(true);
            }
        });
        row.addView(write, wrap());
        body.addView(row, matchWrap());
        if (phone) {
            Button backup = button(getString(R.string.backup_address));
            backup.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    keepName();
                    startActivity(new Intent(WelcomeActivity.this, BackupActivity.class));
                }
            });
            body.addView(backup, wrap());
        }
        Button finish = button(getString(R.string.done));
        finish.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                done(false);
            }
        });
        body.addView(finish, wrap());
    }

    /** The name, when it changed: the mailbox announces it from its next start. */
    private void keepName() {
        if (name == null) {
            return;
        }
        final String typed = name.getText().toString().trim();
        if (typed.equals(Core.str(me, "name"))) {
            return;
        }
        me.put("name", typed);
        new Thread(new Runnable() {
            @Override
            public void run() {
                Core.name(typed);
            }
        }, "loquax-name").start();
    }

    /** On to the conversations, and to a new one when asked. */
    private void done(boolean write) {
        keepName();
        startActivity(new Intent(this, ConversationsActivity.class));
        if (write) {
            startActivity(new Intent(this, NewConversationActivity.class));
        }
        finish();
    }

    private RadioButton option(String words) {
        RadioButton button = new RadioButton(this);
        button.setId(View.generateViewId());
        button.setText(words);
        button.setTextSize(17);
        button.setTypeface(Typeface.DEFAULT_BOLD);
        button.setTextColor(getColor(R.color.text));
        return button;
    }

    private TextView indented(String words) {
        TextView view = text(words, 14, R.color.text_secondary, false);
        view.setPadding(dp(32), 0, 0, dp(8));
        return view;
    }

    private Button button(String words) {
        Button button = new Button(this);
        button.setText(words);
        button.setAllCaps(false);
        return button;
    }

    private TextView text(String words, float sp, int colour, boolean bold) {
        TextView view = new TextView(this);
        view.setText(words);
        view.setTextSize(sp);
        view.setTextColor(getColor(colour));
        if (bold) {
            view.setTypeface(Typeface.DEFAULT_BOLD);
        }
        return view;
    }

    private <V extends View> V gap(V view, int top, int bottom) {
        view.setPadding(view.getPaddingLeft(), dp(top), view.getPaddingRight(), dp(bottom));
        return view;
    }

    private static LinearLayout.LayoutParams matchWrap() {
        return new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    private LinearLayout.LayoutParams wrap() {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        params.topMargin = dp(8);
        params.rightMargin = dp(8);
        return params;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}
