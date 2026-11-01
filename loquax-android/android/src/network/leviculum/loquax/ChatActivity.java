package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.DialogInterface;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Typeface;
import android.graphics.drawable.BitmapDrawable;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.LruCache;
import android.text.Editable;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.TextWatcher;
import android.text.style.ForegroundColorSpan;
import android.text.style.RelativeSizeSpan;
import android.os.Build;
import android.view.Gravity;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.PopupMenu;
import android.widget.ScrollView;
import android.widget.Switch;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * One conversation, after a modern messenger's layout (1129): a bar with
 * the peer's avatar and name; bubbles, ours right and theirs left, grouped
 * when one sender writes several within a few minutes, each with its time
 * and for ours the delivery mark inside; day separators; reactions as pills
 * over a bubble's edge; a pill-shaped compose field with a round send
 * button. Opening it reads the conversation and pulls; leaving it closes
 * it, so what arrives then stays unread.
 *
 * A long press on a bubble (1137) opens Signal's menu (LongPress): the
 * quick reactions with the full picker behind "…", and reply, forward,
 * copy, select, info, pin and delete. What goes on the wire is Rust's
 * (loquax::ui::replies); a pin is this phone's alone.
 *
 * The address book (1139): the bar's menu adds the peer to a contact, or
 * opens, lists and unlinks the one it is linked to, and renames it for
 * every device of the home; an unknown sender's first message asks once
 * in a banner. Picking needs no permission (Book); suggestions ask for
 * READ_CONTACTS, and only when the user taps for them.
 *
 * The contact screen (1146, ContactActivity) opens from a tap on the bar's
 * avatar or name and from the menu's "Contact info", as in Signal; what it
 * hands back runs here: the search in this conversation, which marks and
 * jumps to the newest match and to older ones on each search again, the
 * address book's choice, or closing a blocked or deleted conversation.
 */
public class ChatActivity extends Activity implements Events.Listener {
    static final String PEER = "peer";
    /**
     * The long press's quick reactions: Signal's default six, the same
     * strings as loquax/src/ui/reactions.rs's SIGNAL_QUICK, U+FE0F and all.
     */
    private static final String[] QUICK = {
        "\u2764\uFE0F", "\uD83D\uDC4D", "\uD83D\uDC4E", "\uD83D\uDE02", "\uD83D\uDE2E", "\uD83D\uDE22",
    };
    /** The pins, by peer: the key of the pinned message, this phone only. */
    private static final String PINS = "pins";
    /** The bubble's corner, and a grouped bubble's inner one, in dp. */
    private static final float CORNER = 18;
    private static final float CORNER_GROUPED = 6;

    private final List<Map<String, Object>> rows = new ArrayList<>();
    private String peer;
    /** The own position for the attachment menu's share (1201). */
    private final Locator locator = new Locator(this);
    private TextView title;
    private AvatarView avatar;
    private TextView status;
    private EditText compose;
    private ImageButton send;
    private Adapter adapter;
    private ListView list;
    private boolean homeClient;
    /** The message a reply being written answers, or null. */
    private Map<String, Object> replyTo;
    /**
     * The field is to take the library's draft with the next answer (1219):
     * set whenever the screen comes back. Java keeps no draft of its own; the
     * field mirrors the scene's compose and says every change.
     */
    private boolean draftOwed = true;
    /** The field is being set from the draft, which is no change to say. */
    private boolean mirroring;
    private View replyBar;
    /** The selection's keys while selecting, else null. */
    private Set<Long> selection;
    private View selectionBar;
    private TextView selectionCount;
    private TextView pinned;
    /** The last header the library gave: name, link, banner, petname. */
    private Map<String, Object> header = Core.map();
    private View banner;
    /** Whether the address goes into the new contact the editor is creating. */
    private boolean insertWithAddress;
    /** The contact screen's request code. */
    private static final int CONTACT = 45;
    /** The system picker's answer for the paperclip's "Photo" (1220). */
    private static final int PHOTO = 46;
    /** The system picker's answer for the paperclip's "File" (1221). */
    private static final int FILE = 47;
    /**
     * The most bytes of a picked file kept: past the library's 990 KB cap,
     * so it says the refusal; the rest is only counted.
     */
    private static final int FILE_KEEP = 1000 * 1000;
    /** The longer side of a bubble's photo, in dp. */
    private static final int PHOTO_DP = 240;
    /** The most bytes of a picked picture read; past it the pick is refused. */
    private static final int PICKED_MAX = 64 * 1024 * 1024;

    /** Thumbnails the library drew, by message key: 16 MiB of them. */
    private final LruCache<Long, Bitmap> thumbs = new LruCache<Long, Bitmap>(16 * 1024 * 1024) {
        @Override
        protected int sizeOf(Long key, Bitmap bitmap) {
            return bitmap.getByteCount();
        }
    };
    /** Decodes thumbnails and hands picked pictures over, one at a time. */
    private final ExecutorService photos = Executors.newSingleThreadExecutor();
    /** Voice messages (1222): the coding of one recorded, the player's decoding and writing. */
    private final ExecutorService voices = Executors.newSingleThreadExecutor();
    private final ExecutorService playback = Executors.newSingleThreadExecutor();
    private final Voice.Player player = new Voice.Player();
    /** The recording while the microphone is held, else null. */
    private Voice.Recorder recorder;
    /** Where the finger went down on the microphone, to tell a slide to cancel. */
    private float recordFrom;
    /** The played part of the voice message playing, for its bar when the row is drawn again. */
    private float played;
    private final Runnable recordTick = new Runnable() {
        @Override
        public void run() {
            if (recorder == null) return;
            long ms = recorder.ms();
            if (ms >= Voice.LONGEST_MS) {
                stopRecording(true);
                return;
            }
            long s = ms / 1000;
            line(String.format(Locale.ROOT, "\u25CF %d:%02d   \u2039 %s", s / 60, s % 60,
                    getString(R.string.voice_cancel)), true);
            ticker.postDelayed(this, 200);
        }
    };
    /**
     * The last search in this conversation and the key of its match shown, null for none: a key
     * has either sign (Rust's schema::wire_key), so no number stands for none.
     */
    private String searchQuery = "";
    private Long searchHit = null;
    private final ServiceLink link = new ServiceLink(this);
    /**
     * Asks again while the screen is up: the relative times of the newest
     * group and the library's words (a position's age, the day line) ripen.
     */
    private final Handler ticker = new Handler(Looper.getMainLooper());
    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            refresh();
            ticker.postDelayed(this, 30_000);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_chat);
        peer = getIntent().getStringExtra(PEER);
        title = (TextView) findViewById(R.id.title);
        avatar = (AvatarView) findViewById(R.id.avatar);
        status = (TextView) findViewById(R.id.status);
        compose = (EditText) findViewById(R.id.compose);
        send = (ImageButton) findViewById(R.id.send);
        int text = getColor(R.color.text);
        ImageButton back = (ImageButton) findViewById(R.id.back);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, text));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        View.OnClickListener contact = new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                openContact();
            }
        };
        avatar.setOnClickListener(contact);
        title.setOnClickListener(contact);
        final ImageButton menu = (ImageButton) findViewById(R.id.menu);
        menu.setImageDrawable(new Glyph(Glyph.Kind.MENU, text));
        menu.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showMenu(menu);
            }
        });
        ImageButton emoji = (ImageButton) findViewById(R.id.emoji);
        emoji.setImageDrawable(new Glyph(Glyph.Kind.EMOJI, getColor(R.color.text_secondary)));
        emoji.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pickEmoji();
            }
        });
        final ImageButton attach = (ImageButton) findViewById(R.id.attach);
        attach.setImageDrawable(new Glyph(Glyph.Kind.ATTACH, getColor(R.color.text_secondary)));
        attach.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                attach(attach);
            }
        });
        replyBar = findViewById(R.id.reply_bar);
        ImageButton cancel = (ImageButton) findViewById(R.id.reply_cancel);
        cancel.setImageDrawable(new Glyph(Glyph.Kind.CLOSE, getColor(R.color.text_secondary)));
        cancel.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                replyTo(null);
            }
        });
        selectionBar = findViewById(R.id.selection_bar);
        selectionCount = (TextView) findViewById(R.id.selection_count);
        ImageButton close = (ImageButton) findViewById(R.id.selection_close);
        close.setImageDrawable(new Glyph(Glyph.Kind.CLOSE, text));
        close.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                select(null);
            }
        });
        ((Button) findViewById(R.id.selection_copy)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                copy(selected());
                select(null);
            }
        });
        ((Button) findViewById(R.id.selection_delete)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                askDelete(selected());
            }
        });
        banner = findViewById(R.id.book_banner);
        ((Button) findViewById(R.id.book_add)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                choose();
            }
        });
        ((Button) findViewById(R.id.book_not_now)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                say(Core.notNow(peer));
                refresh();
            }
        });
        if (savedInstanceState != null) {
            insertWithAddress = savedInstanceState.getBoolean("insertWithAddress");
        }
        pinned = (TextView) findViewById(R.id.pinned);
        pinned.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                goToPinned();
            }
        });
        pinned.setOnLongClickListener(new View.OnLongClickListener() {
            @Override
            public boolean onLongClick(View v) {
                pin(0);
                return true;
            }
        });
        send.setImageDrawable(new Glyph(Glyph.Kind.SEND, getColor(R.color.on_send)));
        compose.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                sendable();
                tellDraft();
            }
        });
        sendable();
        list = (ListView) findViewById(R.id.list);
        adapter = new Adapter();
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                if (selection != null) {
                    toggle(rows.get(position));
                } else {
                    showFix(rows.get(position));
                }
            }
        });
        list.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                if (selection != null) {
                    toggle(rows.get(position));
                } else {
                    longPress(view, rows.get(position));
                }
                return true;
            }
        });
        send.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                send();
            }
        });
        // With nothing typed the button is the microphone (1222), as in
        // Signal: held, it records; let go, the message goes; slid left,
        // it is gone.
        send.setOnTouchListener(new View.OnTouchListener() {
            @Override
            public boolean onTouch(View v, android.view.MotionEvent event) {
                if (!compose.getText().toString().trim().isEmpty()) {
                    return false;
                }
                switch (event.getActionMasked()) {
                    case android.view.MotionEvent.ACTION_DOWN:
                        recordFrom = event.getRawX();
                        startRecording();
                        return true;
                    case android.view.MotionEvent.ACTION_MOVE:
                        if (recorder != null && recordFrom - event.getRawX() > dp(96)) {
                            stopRecording(false);
                        }
                        return true;
                    case android.view.MotionEvent.ACTION_UP:
                        stopRecording(true);
                        return true;
                    case android.view.MotionEvent.ACTION_CANCEL:
                        stopRecording(false);
                        return true;
                    default:
                        return true;
                }
            }
        });
    }

    /** The microphone pressed: the permission first, at its first use. */
    private void startRecording() {
        if (checkSelfPermission(android.Manifest.permission.RECORD_AUDIO)
                != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {android.Manifest.permission.RECORD_AUDIO}, Voice.ASK);
            return;
        }
        player.stop();
        Voice.Recorder started = new Voice.Recorder(this);
        if (!started.start()) {
            line(getString(R.string.voice_no_microphone));
            return;
        }
        recorder = started;
        send.setPressed(true);
        send.performHapticFeedback(android.view.HapticFeedbackConstants.VIRTUAL_KEY);
        ticker.post(recordTick);
    }

    /** The microphone let go (`send`) or slid away: the recording goes to the library, or nowhere. */
    private void stopRecording(boolean sendIt) {
        final Voice.Recorder done = recorder;
        if (done == null) return;
        recorder = null;
        ticker.removeCallbacks(recordTick);
        send.setPressed(false);
        final byte[] pcm = done.stop();
        final int rate = done.rate();
        line("");
        if (!sendIt) return;
        voices.execute(new Runnable() {
            @Override
            public void run() {
                final String error = Core.voice(peer, pcm, rate);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        say(error);
                        refresh();
                    }
                });
            }
        });
    }

    @Override
    protected void onSaveInstanceState(Bundle out) {
        super.onSaveInstanceState(out);
        out.putBoolean("insertWithAddress", insertWithAddress);
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
        Events.listen(this);
        String error = Core.open(peer);
        if (Ui.NOT_STARTED.equals(error)) {
            finish();
            return;
        }
        Core.front(true);
        say(error);
        draftOwed = true;
        refresh();
        ticker.postDelayed(tick, 30_000);
    }

    @Override
    protected void onPause() {
        ticker.removeCallbacks(tick);
        stopRecording(false);
        player.stop();
        Core.front(false);
        Events.listen(null);
        Core.close();
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        // A picked picture already handed over still goes; thumbnails
        // still owed are not drawn.
        photos.shutdown();
        voices.shutdown();
        player.stop();
        playback.shutdownNow();
        super.onDestroy();
    }

    /** Back ends a selection first, then a reply being written, then the screen. */
    @Override
    public void onBackPressed() {
        if (selection != null) {
            select(null);
        } else if (replyTo != null) {
            replyTo(null);
        } else {
            super.onBackPressed();
        }
    }

    private void say(String error) {
        if (error != null) {
            line(error);
        }
    }

    /** The line under the bar, shown only while it has something to say. */
    private void line(String text) {
        Ui.showLine(status, text, true);
    }

    private void line(String text, boolean stays) {
        Ui.showLine(status, text, stays);
    }

    /** The send button is bright with text to send and dimmed without. */
    private void sendable() {
        boolean some = !compose.getText().toString().trim().isEmpty();
        // Nothing typed: the microphone (1222), which is always there.
        send.setImageDrawable(new Glyph(some ? Glyph.Kind.SEND : Glyph.Kind.MIC,
                getColor(R.color.on_send)));
        send.setContentDescription(getString(some ? R.string.send : R.string.record_voice));
        send.setEnabled(true);
        send.setAlpha(1f);
    }

    /** The bar's menu: pull now, the map, the address book and the name. */
    private void showMenu(View anchor) {
        PopupMenu popup = new PopupMenu(this, anchor);
        popup.getMenu().add(0, R.string.contact_info, 0, R.string.contact_info);
        popup.getMenu().add(0, R.string.chat_search, 0, R.string.chat_search);
        popup.getMenu().add(0, R.string.refresh, 0, R.string.refresh);
        popup.getMenu().add(0, R.string.map, 1, R.string.map);
        if (Core.bool(header, "linked")) {
            popup.getMenu().add(0, R.string.open_contact, 2, R.string.open_contact);
            Object others = header.get("others");
            if (others instanceof List && !((List<?>) others).isEmpty()) {
                popup.getMenu().add(0, R.string.contact_addresses, 3, R.string.contact_addresses);
            }
            popup.getMenu().add(0, R.string.unlink, 4, R.string.unlink);
        } else {
            popup.getMenu().add(0, R.string.add_to_contacts, 2, R.string.add_to_contacts);
        }
        popup.getMenu().add(0, R.string.rename, 5, R.string.rename);
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                int id = item.getItemId();
                if (id == R.string.contact_info) {
                    openContact();
                } else if (id == R.string.chat_search) {
                    search();
                } else if (id == R.string.refresh) {
                    say(Core.pull());
                } else if (id == R.string.map) {
                    startActivity(new Intent(ChatActivity.this, MapActivity.class));
                } else if (id == R.string.open_contact) {
                    if (!Book.open(ChatActivity.this, Core.num(header, "contact_id"),
                            Core.str(header, "lookup"))) {
                        line(getString(R.string.no_contacts_app));
                    }
                } else if (id == R.string.contact_addresses) {
                    addresses();
                } else if (id == R.string.unlink) {
                    say(Core.unlink(peer));
                    refresh();
                } else if (id == R.string.add_to_contacts) {
                    choose();
                } else if (id == R.string.rename) {
                    rename();
                }
                return true;
            }
        });
        popup.show();
    }

    /**
     * The choice of a contact, titled with the peer as every screen names
     * it: with READ_CONTACTS up to five suggestions the library ranked from
     * the announced name, then another contact by the picker, a new one,
     * and "Not now"; without it, a line that asks for the permission.
     */
    private void choose() {
        final List<String> labels = new ArrayList<>();
        final List<Runnable> actions = new ArrayList<>();
        if (Book.canRead(this)) {
            List<Map<String, Object>> candidates = Book.candidates(getContentResolver());
            Map<String, String> display = new java.util.HashMap<>();
            for (Map<String, Object> candidate : candidates) {
                display.put(Core.str(candidate, "key"), Core.str(candidate, "display"));
            }
            for (final Map<String, Object> suggestion : Core.suggest(peer, candidates)) {
                final String key = Core.str(suggestion, "key");
                labels.add(display.get(key) + " \u00B7 " + matched(suggestion));
                actions.add(new Runnable() {
                    @Override
                    public void run() {
                        Map<String, Object> copy = Book.copyByKey(getContentResolver(), key);
                        if (copy == null) {
                            line(getString(R.string.contact_unreadable));
                        } else {
                            confirm(copy);
                        }
                    }
                });
            }
        }
        labels.add(getString(R.string.another_contact));
        actions.add(new Runnable() {
            @Override
            public void run() {
                if (!Book.pick(ChatActivity.this)) {
                    line(getString(R.string.no_contacts_app));
                }
            }
        });
        labels.add(getString(R.string.new_contact));
        actions.add(new Runnable() {
            @Override
            public void run() {
                askNew();
            }
        });
        if (!Book.canRead(this)) {
            labels.add(getString(R.string.allow_suggestions));
            actions.add(new Runnable() {
                @Override
                public void run() {
                    Book.ask(ChatActivity.this);
                }
            });
        }
        labels.add(getString(R.string.not_now));
        actions.add(new Runnable() {
            @Override
            public void run() {
                say(Core.notNow(peer));
                refresh();
            }
        });
        new AlertDialog.Builder(this)
                .setTitle(Core.str(header, "name"))
                .setItems(labels.toArray(new String[0]), new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        actions.get(which).run();
                    }
                })
                .show();
    }

    /** What matched, in words: "nickname Ali". */
    private String matched(Map<String, Object> suggestion) {
        String field = Core.str(suggestion, "field");
        if ("nickname".equals(field)) {
            return getString(R.string.matched_nickname, Core.str(suggestion, "nickname"));
        } else if ("given".equals(field)) {
            return getString(R.string.matched_given);
        } else if ("words".equals(field)) {
            return getString(R.string.matched_words);
        } else if ("prefix".equals(field)) {
            return getString(R.string.matched_prefix);
        }
        return getString(R.string.matched_display);
    }

    /**
     * The attachment menu (1201): a photo from the system's picker (1220),
     * any file from it (1221), the own position, sent once to this chat when the user picks it;
     * the permission first if need be.
     */
    private void attach(View anchor) {
        PopupMenu popup = new PopupMenu(this, anchor);
        popup.getMenu().add(0, R.string.attach_photo, 0, R.string.attach_photo);
        popup.getMenu().add(0, R.string.attach_file, 0, R.string.attach_file);
        popup.getMenu().add(0, R.string.attach_position, 0, R.string.attach_position);
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                if (item.getItemId() == R.string.attach_photo) {
                    // The system's picker, no camera yet (1220); the
                    // picked bytes go to the library as they are.
                    startActivityForResult(new Intent(Intent.ACTION_GET_CONTENT)
                            .setType("image/*")
                            .addCategory(Intent.CATEGORY_OPENABLE), PHOTO);
                } else if (item.getItemId() == R.string.attach_file) {
                    // Any file the system's picker offers (1221).
                    startActivityForResult(new Intent(Intent.ACTION_GET_CONTENT)
                            .setType("*/*")
                            .addCategory(Intent.CATEGORY_OPENABLE), FILE);
                } else if (item.getItemId() == R.string.attach_position) {
                    locator.shareWith(peer, Core.str(header, "name"), new Locator.Fix() {
                        @Override
                        public void on(android.location.Location location) {
                            refresh();
                        }
                    }, new Locator.Say() {
                        @Override
                        public void line(String text) {
                            ChatActivity.this.line(text);
                        }
                    });
                }
                return true;
            }
        });
        popup.show();
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(code, permissions, results);
        if (locator.result(code)) {
            return;
        }
        if (code == Voice.ASK) {
            // Granted: the next press records. Refused: why it does not.
            if (results.length == 0 || results[0]
                    != android.content.pm.PackageManager.PERMISSION_GRANTED) {
                line(getString(R.string.voice_no_microphone));
            } else {
                line(getString(R.string.voice_hold));
            }
            return;
        }
        if (code == Book.ASK && Book.canRead(this)) {
            Book.observe(this);
            choose();
        }
        // Refused: the line is gone from the sheet; the phone's settings
        // can still grant it.
    }

    /** The switch "Also save the address in the contact", off, and its one sentence. */
    private Switch saveSwitch(LinearLayout into) {
        Switch save = new Switch(this);
        save.setText(R.string.save_address);
        save.setChecked(false);
        save.setPadding(0, dp(12), 0, 0);
        into.addView(save);
        TextView why = new TextView(this);
        why.setText(R.string.save_address_why);
        why.setTextSize(13);
        why.setTextColor(getColor(R.color.text_secondary));
        why.setPadding(0, dp(4), 0, 0);
        into.addView(why);
        return save;
    }

    private LinearLayout dialogBody(String message) {
        LinearLayout body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(dp(24), dp(12), dp(24), 0);
        TextView text = new TextView(this);
        text.setText(message);
        text.setTextSize(16);
        text.setTextColor(getColor(R.color.text));
        body.addView(text);
        return body;
    }

    /**
     * "Link Alice ·4f2a to Alice Schmidt?" with the contact's photo, the
     * contact's other addresses named, and the switch. The link stands
     * whatever the editor does after.
     */
    private void confirm(final Map<String, Object> copy) {
        final String contactName = Core.str(copy, "name");
        StringBuilder message = new StringBuilder(
                getString(R.string.link_to, Core.str(header, "name"), contactName));
        List<String> others = new ArrayList<>();
        for (Map<String, Object> row : Core.contact(Core.str(copy, "lookup"))) {
            if (!peer.equals(Core.str(row, "peer"))) {
                others.add(Core.str(row, "name"));
            }
        }
        if (!others.isEmpty()) {
            message.append("\n\n").append(getString(R.string.other_addresses, contactName,
                    android.text.TextUtils.join(", ", others)));
        }
        LinearLayout body = dialogBody(message.toString());
        final Switch save = saveSwitch(body);
        AlertDialog.Builder dialog = new AlertDialog.Builder(this)
                .setView(body)
                .setPositiveButton(R.string.link, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        String error = Core.link(peer, copy);
                        if (error != null) {
                            line(error);
                            return;
                        }
                        line(getString(R.string.linked_to, contactName));
                        refresh();
                        if (save.isChecked() && !Book.saveAddress(ChatActivity.this,
                                Core.num(copy, "contact_id"), Core.str(copy, "lookup"), peer)) {
                            line(getString(R.string.no_contacts_app));
                        }
                    }
                })
                .setNegativeButton(android.R.string.cancel, null);
        Object photo = copy.get("photo");
        if (photo instanceof byte[]) {
            byte[] bytes = (byte[]) photo;
            android.graphics.Bitmap bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.length);
            if (bitmap != null) {
                dialog.setIcon(new BitmapDrawable(getResources(), bitmap));
            }
        }
        dialog.show();
    }

    /** A new contact with the announced name, the address in it when the switch says so. */
    private void askNew() {
        String announced = Core.str(header, "announced");
        LinearLayout body = dialogBody(getString(R.string.new_contact_for, Core.str(header, "name")));
        final Switch save = saveSwitch(body);
        final String name = announced;
        new AlertDialog.Builder(this)
                .setView(body)
                .setPositiveButton(R.string.create, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        insertWithAddress = save.isChecked();
                        if (!Book.insert(ChatActivity.this, name, peer, insertWithAddress)) {
                            line(getString(R.string.no_contacts_app));
                        }
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    /**
     * The picker's contact, read now while its grant lasts, goes to the
     * confirmation; a contact the editor created is linked as it is: the
     * user just made it for this peer. Backed out, nothing changes.
     */
    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == CONTACT) {
            String then = result == RESULT_OK && data != null
                    ? data.getStringExtra(ContactActivity.THEN) : null;
            if (ContactActivity.THEN_SEARCH.equals(then)) {
                search();
            } else if (ContactActivity.THEN_LINK.equals(then)) {
                choose();
            } else if (ContactActivity.THEN_CLOSE.equals(then)) {
                finish();
            }
            return;
        }
        if (result != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        if (request == PHOTO) {
            sendPhoto(data.getData());
            return;
        }
        if (request == FILE) {
            sendFile(data.getData());
            return;
        }
        if (request != Book.PICK && request != Book.INSERT) {
            return;
        }
        Map<String, Object> copy = Book.copy(getContentResolver(), data.getData());
        if (copy == null) {
            line(getString(R.string.contact_unreadable));
            return;
        }
        if (request == Book.PICK) {
            confirm(copy);
            return;
        }
        String error = Core.link(peer, copy);
        line(error != null ? error : getString(R.string.linked_to, Core.str(copy, "name")));
        refresh();
    }

    /**
     * The picked picture's bytes, read off the UI thread, to the library,
     * which scales, strips and sends it (1220); what it refuses is said.
     */
    private void sendPhoto(final android.net.Uri uri) {
        photos.execute(new Runnable() {
            @Override
            public void run() {
                String error;
                try (java.io.InputStream in = getContentResolver().openInputStream(uri)) {
                    java.io.ByteArrayOutputStream bytes = new java.io.ByteArrayOutputStream();
                    byte[] chunk = new byte[64 * 1024];
                    int n;
                    while (in != null && (n = in.read(chunk)) > 0 && bytes.size() <= PICKED_MAX) {
                        bytes.write(chunk, 0, n);
                    }
                    error = bytes.size() > PICKED_MAX
                            ? getString(R.string.photo_unreadable)
                            : Core.photo(peer, bytes.toByteArray());
                } catch (java.io.IOException | SecurityException e) {
                    error = getString(R.string.photo_unreadable);
                }
                final String said = error;
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (said != null) {
                            line(said);
                        }
                        refresh();
                    }
                });
            }
        });
    }

    /**
     * The picked file's name and bytes, read off the UI thread, to the
     * library, which sends it or says why not (1221): past FILE_KEEP only
     * counted, so a film is not held in memory to be refused.
     */
    private void sendFile(final android.net.Uri uri) {
        photos.execute(new Runnable() {
            @Override
            public void run() {
                String name = "";
                try (android.database.Cursor cursor = getContentResolver().query(uri,
                        new String[] {android.provider.OpenableColumns.DISPLAY_NAME},
                        null, null, null)) {
                    if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                        name = cursor.getString(0);
                    }
                } catch (RuntimeException e) {
                    name = "";
                }
                if (name.isEmpty() && uri.getLastPathSegment() != null) {
                    name = uri.getLastPathSegment();
                }
                String error;
                try (java.io.InputStream in = getContentResolver().openInputStream(uri)) {
                    java.io.ByteArrayOutputStream bytes = new java.io.ByteArrayOutputStream();
                    byte[] chunk = new byte[64 * 1024];
                    long count = 0;
                    int n;
                    while (in != null && (n = in.read(chunk)) > 0) {
                        int keep = (int) Math.max(0, Math.min(n, FILE_KEEP - count));
                        bytes.write(chunk, 0, keep);
                        count += n;
                    }
                    error = in == null
                            ? getString(R.string.file_unreadable)
                            : Core.file(peer, name, bytes.toByteArray(),
                                    count > bytes.size() ? count : 0);
                } catch (java.io.IOException | SecurityException e) {
                    error = getString(R.string.file_unreadable);
                }
                final String said = error;
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (said != null) {
                            line(said);
                        }
                        refresh();
                    }
                });
            }
        });
    }

    /**
     * A row's files (1221): one line each in its bubble (1268), its name
     * and size in the bubble's ink, said as the library words it; a tap
     * opens it, a long press shares it. Gone for a row without.
     */
    private void files(LinearLayout box, Map<String, Object> row, boolean ours) {
        box.removeAllViews();
        List<Map<String, Object>> files = Core.list(row, "files");
        box.setVisibility(files.isEmpty() ? View.GONE : View.VISIBLE);
        final long key = Core.num(row, "key");
        for (int i = 0; i < files.size(); i++) {
            Map<String, Object> file = files.get(i);
            final int index = i;
            TextView line = new TextView(this);
            line.setText("\u25A4  " + Core.str(file, "name") + "\n" + Core.str(file, "size"));
            line.setTextSize(15);
            line.setTextColor(getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text));
            line.setPadding(0, dp(2), 0, dp(2));
            line.setMaxWidth(dp(280));
            line.setContentDescription(Core.str(file, "words"));
            LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            params.bottomMargin = dp(2);
            line.setLayoutParams(params);
            line.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    openFile(key, index, false);
                }
            });
            line.setOnLongClickListener(new View.OnLongClickListener() {
                @Override
                public boolean onLongClick(View v) {
                    openFile(key, index, true);
                    return true;
                }
            });
            box.addView(line);
        }
    }

    /**
     * A row's voice message (1222): the play or pause button, the level
     * bar and the length in its bubble (1268), in the bubble's ink, said
     * as the library words it; a tap plays or pauses it. Gone for a row
     * without.
     */
    private void voice(LinearLayout box, final Map<String, Object> row, boolean ours) {
        box.removeAllViews();
        if (Core.num(row, "voice_mode") == 0) {
            box.setVisibility(View.GONE);
            box.setTag(null);
            return;
        }
        final long key = Core.num(row, "key");
        box.setVisibility(View.VISIBLE);
        box.setTag("voice_" + key);
        int ink = getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text);
        box.setContentDescription(Core.str(row, "voice_words"));
        boolean playing = Core.bool(row, "voice_playing") && player.playing() == key;
        ImageButton toggle = new ImageButton(this);
        toggle.setImageDrawable(new Glyph(playing ? Glyph.Kind.PAUSE : Glyph.Kind.PLAY, ink));
        toggle.setBackground(null);
        toggle.setPadding(dp(8), dp(8), dp(8), dp(8));
        toggle.setContentDescription(getString(playing ? R.string.scene_pause_voice
                : R.string.scene_play_voice));
        toggle.setEnabled(Core.bool(row, "voice_playable"));
        toggle.setAlpha(Core.bool(row, "voice_playable") ? 1f : 0.38f);
        toggle.setLayoutParams(new LinearLayout.LayoutParams(dp(44), dp(44)));
        toggle.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                toggleVoice(key);
            }
        });
        box.addView(toggle);
        Voice.Bar bar = new Voice.Bar(this);
        Object levels = row.get("voice_levels");
        bar.set(levels instanceof byte[] ? (byte[]) levels : null, ink, playing ? played : 0f);
        bar.setTag("voice_bar");
        LinearLayout.LayoutParams barSize = new LinearLayout.LayoutParams(dp(150), dp(28));
        barSize.setMarginStart(dp(4));
        barSize.setMarginEnd(dp(10));
        bar.setLayoutParams(barSize);
        box.addView(bar);
        TextView length = new TextView(this);
        length.setText(Core.str(row, "voice_duration"));
        length.setTextSize(13);
        length.setTextColor(ink);
        box.addView(length);
    }

    /** A tap on a voice message's button: it plays, or the one playing stops. */
    private void toggleVoice(final long key) {
        if (player.playing() == key) {
            player.stop();
            return;
        }
        player.stop();
        say(Core.playVoice(peer, key, true));
        played = 0f;
        refresh();
        playback.execute(new Runnable() {
            @Override
            public void run() {
                Map<String, Object> sound = Core.voicePcm(peer, key);
                Object pcm = sound.get("pcm");
                if (!Core.bool(sound, "ok") || !(pcm instanceof byte[])) {
                    final String error = Core.str(sound, "error");
                    runOnUiThread(new Runnable() {
                        @Override
                        public void run() {
                            Core.playVoice(peer, key, false);
                            line(error);
                            refresh();
                        }
                    });
                    return;
                }
                player.play(key, (byte[]) pcm, (int) Core.num(sound, "rate"), new Voice.Listener() {
                    @Override
                    public void progress(long at, float done) {
                        played = done;
                        View box = list.findViewWithTag("voice_" + at);
                        View bar = box == null ? null : box.findViewWithTag("voice_bar");
                        if (bar instanceof Voice.Bar) ((Voice.Bar) bar).progress(done);
                    }

                    @Override
                    public void ended(long at) {
                        played = 0f;
                        Core.playVoice(peer, at, false);
                        refresh();
                    }
                });
            }
        });
    }

    /**
     * The library saves the file under the app's files (1221); the app the
     * system has for its kind opens it, or the share sheet takes it, with
     * read access to that one file (FilesProvider).
     */
    private void openFile(final long key, final int index, final boolean share) {
        photos.execute(new Runnable() {
            @Override
            public void run() {
                final Map<String, Object> saved = Core.saveFile(peer, key, index);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        android.net.Uri uri = Core.bool(saved, "ok")
                                ? FilesProvider.uri(ChatActivity.this, Core.str(saved, "path"))
                                : null;
                        if (uri == null) {
                            line(getString(R.string.file_unreadable));
                            return;
                        }
                        String name = Core.str(saved, "name");
                        String type = FilesProvider.type(name);
                        Intent send = new Intent(Intent.ACTION_SEND)
                                .setType(type)
                                .putExtra(Intent.EXTRA_STREAM, uri)
                                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                        send.setClipData(ClipData.newRawUri(name, uri));
                        Intent chooser = Intent.createChooser(send, name)
                                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                        Intent view = new Intent(Intent.ACTION_VIEW)
                                .setDataAndType(uri, type)
                                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                        try {
                            startActivity(share ? chooser : view);
                        } catch (android.content.ActivityNotFoundException e) {
                            try {
                                startActivity(chooser);
                            } catch (android.content.ActivityNotFoundException again) {
                                line(getString(R.string.file_no_app));
                            }
                        }
                    }
                });
            }
        });
    }

    /**
     * A row's photo (1220): sized to its shape within PHOTO_DP, the
     * library's thumbnail from the cache or drawn now, a tap to the whole
     * photo; gone for a row without one.
     */
    private void photo(final ImageView view, Map<String, Object> row) {
        if (Core.str(row, "photo").isEmpty()) {
            view.setVisibility(View.GONE);
            view.setTag(null);
            view.setImageDrawable(null);
            view.setOnClickListener(null);
            return;
        }
        final long key = Core.num(row, "key");
        long w = Core.num(row, "photo_w");
        long h = Core.num(row, "photo_h");
        int box = dp(PHOTO_DP);
        int vw = box;
        int vh = box;
        if (w > 0 && h > 0) {
            if (w >= h) {
                vh = (int) Math.max(dp(48), box * h / w);
            } else {
                vw = (int) Math.max(dp(48), box * w / h);
            }
        }
        ViewGroup.LayoutParams params = view.getLayoutParams();
        params.width = vw;
        params.height = vh;
        view.setLayoutParams(params);
        view.setVisibility(View.VISIBLE);
        view.setContentDescription(Core.str(row, "photo_words"));
        view.setTag(key);
        Bitmap cached = thumbs.get(key);
        if (cached != null) {
            view.setImageBitmap(cached);
        } else {
            view.setImageDrawable(new android.graphics.drawable.ColorDrawable(
                    getColor(R.color.bubble_theirs)));
            final int edge = Math.max(vw, vh);
            photos.execute(new Runnable() {
                @Override
                public void run() {
                    final Bitmap bitmap = Core.photoBitmap(peer, key, edge);
                    runOnUiThread(new Runnable() {
                        @Override
                        public void run() {
                            if (bitmap == null) {
                                return;
                            }
                            thumbs.put(key, bitmap);
                            if (Long.valueOf(key).equals(view.getTag())) {
                                view.setImageBitmap(bitmap);
                            }
                        }
                    });
                }
            });
        }
        view.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(ChatActivity.this, PhotoActivity.class)
                        .putExtra(PhotoActivity.PEER, peer)
                        .putExtra(PhotoActivity.KEY, key));
            }
        });
    }

    /** The contact screen of this conversation's peer (1146). */
    private void openContact() {
        startActivityForResult(new Intent(this, ContactActivity.class)
                .putExtra(ContactActivity.PEER, peer), CONTACT);
    }

    /**
     * Search the messages shown for a text: the newest match is marked and
     * scrolled to, and the same text again goes to the next older one.
     */
    private void search() {
        final EditText field = new EditText(this);
        field.setSingleLine(true);
        field.setHint(R.string.chat_search_hint);
        field.setText(searchQuery);
        field.setSelection(field.getText().length());
        LinearLayout body = new LinearLayout(this);
        body.setPadding(dp(20), dp(8), dp(20), 0);
        body.addView(field, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        new AlertDialog.Builder(this)
                .setTitle(R.string.chat_search)
                .setView(body)
                .setPositiveButton(R.string.chat_search, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        find(field.getText().toString().trim());
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    private void find(String query) {
        if (!query.equals(searchQuery)) {
            searchHit = null;
        }
        searchQuery = query;
        if (query.isEmpty()) {
            searchHit = null;
            adapter.notifyDataSetChanged();
            return;
        }
        String needle = query.toLowerCase(Locale.getDefault());
        List<Integer> matches = new ArrayList<>();
        for (int i = rows.size() - 1; i >= 0; i--) {
            if (Core.str(rows.get(i), "body").toLowerCase(Locale.getDefault()).contains(needle)) {
                matches.add(i);
            }
        }
        if (matches.isEmpty()) {
            searchHit = null;
            line(getString(R.string.chat_search_none));
            adapter.notifyDataSetChanged();
            return;
        }
        // The next older match than the one shown, the newest again after the oldest.
        int next = 0;
        for (int m = 0; m < matches.size(); m++) {
            if (Long.valueOf(Core.num(rows.get(matches.get(m)), "key")).equals(searchHit)) {
                next = (m + 1) % matches.size();
                break;
            }
        }
        int index = matches.get(next);
        searchHit = Core.num(rows.get(index), "key");
        list.setSelection(index);
        line(getString(R.string.chat_search_found, next + 1, matches.size()));
        adapter.notifyDataSetChanged();
    }

    /** The user's own name for the peer, for every device of the home; empty clears it. */
    private void rename() {
        final EditText field = new EditText(this);
        field.setSingleLine(true);
        field.setText(Core.str(header, "petname"));
        field.setSelection(field.getText().length());
        field.setHint(R.string.rename_hint);
        LinearLayout body = new LinearLayout(this);
        body.setPadding(dp(20), dp(8), dp(20), 0);
        body.addView(field, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        new AlertDialog.Builder(this)
                .setTitle(R.string.rename)
                .setView(body)
                .setPositiveButton(android.R.string.ok, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        say(Core.rename(peer, field.getText().toString()));
                        refresh();
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    /** The linked contact's addresses, each with its own chat. */
    private void addresses() {
        final List<Map<String, Object>> rows = Core.contact(Core.str(header, "lookup"));
        String[] names = new String[rows.size()];
        for (int i = 0; i < rows.size(); i++) {
            names[i] = Core.str(rows.get(i), "name");
        }
        new AlertDialog.Builder(this)
                .setTitle(R.string.contact_addresses)
                .setItems(names, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        String other = Core.str(rows.get(which), "peer");
                        if (!other.equals(peer)) {
                            startActivity(new Intent(ChatActivity.this, ChatActivity.class)
                                    .putExtra(PEER, other));
                        }
                    }
                })
                .show();
    }

    /** An emoji from the full picker into the compose field, at the cursor. */
    private void pickEmoji() {
        EmojiPicker.show(this, new EmojiPicker.Listener() {
            @Override
            public void picked(String emoji) {
                int at = Math.max(compose.getSelectionStart(), 0);
                int end = Math.max(compose.getSelectionEnd(), 0);
                compose.getText().replace(Math.min(at, end), Math.max(at, end), emoji);
                say(Core.used(emoji));
            }
        });
    }

    private int dp(float dp) {
        return Math.round(dp * getResources().getDisplayMetrics().density);
    }

    private void send() {
        String body = compose.getText().toString();
        if (body.trim().isEmpty()) {
            return;
        }
        String error = replyTo == null
                ? Core.send(peer, body) : Core.reply(peer, Core.str(replyTo, "id"), body);
        if (error != null) {
            line(error);
            return;
        }
        // The library dropped the draft with the send (1219).
        mirroring = true;
        try {
            compose.setText("");
            replyTo(null);
        } finally {
            mirroring = false;
        }
        refresh();
    }

    /**
     * A message that carries a position, a report or a text, opens the map
     * centred on its fix with the sender's pin selected, as Enter does in
     * the terminal. Back comes here again where the list was.
     */
    private void showFix(Map<String, Object> row) {
        if (row.get("lat_e6") == null || row.get("lon_e6") == null) {
            return;
        }
        if (!Core.str(row, "live").isEmpty()) {
            // The newest report's line, which says whether the position is
            // alive (1209), opens the map as the list's pin does (1224):
            // the Map tab on the friend, Back to the chats.
            ConversationsActivity.openMapAt(this, peer);
            return;
        }
        startActivity(new Intent(this, MapActivity.class)
                .putExtra(MapActivity.LAT_E6, Core.num(row, "lat_e6"))
                .putExtra(MapActivity.LON_E6, Core.num(row, "lon_e6"))
                .putExtra(MapActivity.SELECTED, peer));
    }

    /**
     * The bubble's text: the body, and under it, for a message that carries
     * a position, its line (1132): a pin, the coordinates, the accuracy
     * when the sender gave one, how old the fix is; dimmed once it is
     * stale. A report has no body: its line is the bubble.
     */
    private CharSequence text(Map<String, Object> row, boolean ours, long now) {
        String body = Core.str(row, "body");
        if (row.get("place") == null) {
            return body;
        }
        StringBuilder line = new StringBuilder("\uD83D\uDCCD ").append(Core.str(row, "place"));
        String accuracy = Core.str(row, "accuracy");
        if (!accuracy.isEmpty()) {
            line.append(" \u00B7 ").append(accuracy);
        }
        String live = Core.str(row, "live");
        if (!live.isEmpty()) {
            // The newest report says whether the position is alive (1209),
            // in the library's words (1217).
            line.append(" \u00B7 ").append(live);
        } else if (row.get("fix_at") != null) {
            line.append(" \u00B7 ").append(Ui.age(this, Core.num(row, "fix_at"), now));
        }
        // An older run stands for all of it (1184): how many, since when.
        long updates = Core.num(row, "updates");
        if (updates > 1 && live.isEmpty()) {
            String since = android.text.format.DateFormat.getTimeFormat(this)
                    .format(new java.util.Date(Core.num(row, "since") * 1000));
            line.append(" \u00B7 ").append(getString(R.string.run_updates, (int) updates, since));
        }
        SpannableStringBuilder out = new SpannableStringBuilder(body);
        if (out.length() > 0) {
            out.append('\n');
        }
        int start = out.length();
        out.append(line);
        int end = out.length();
        out.setSpan(new RelativeSizeSpan(0.88f), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        boolean stale = row.get("stale_at") != null && now > Core.num(row, "stale_at");
        if (stale) {
            int dim = getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
            out.setSpan(new ForegroundColorSpan(dim), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        }
        return out;
    }

    /** Our reaction on `row`, or null: the chip that is ours. */
    private static String ourReaction(Map<String, Object> row) {
        for (Map<String, Object> chip : Core.list(row, "chips")) {
            if (Core.bool(chip, "ours")) {
                return Core.str(chip, "content");
            }
        }
        return null;
    }

    /** The long press on the bubble of `row`, in the row's `view`. */
    private void longPress(View view, final Map<String, Object> row) {
        final boolean hasId = !Core.str(row, "id").isEmpty();
        boolean isPinned = pinnedKey() == Core.num(row, "key");
        int[] actions = {
            R.string.action_reply, R.string.action_forward, R.string.action_copy,
            R.string.action_select, R.string.action_info,
            isPinned ? R.string.action_unpin : R.string.action_pin, R.string.action_delete,
        };
        CharSequence[] labels = null;
        long updates = Core.num(row, "updates");
        if (updates > 1) {
            // A run's row (1184): every report it folds, first in the list.
            int[] more = new int[actions.length + 1];
            more[0] = R.string.action_run;
            System.arraycopy(actions, 0, more, 1, actions.length);
            actions = more;
            labels = new CharSequence[actions.length];
            labels[0] = getString(R.string.action_run, (int) updates);
        }
        LongPress.show(this, view.findViewById(R.id.bubble), Core.bool(row, "ours"), hasId,
                QUICK, ourReaction(row), actions, labels, new LongPress.Listener() {
                    @Override
                    public void react(String emoji) {
                        say(Core.react(peer, Core.str(row, "id"), emoji));
                    }

                    @Override
                    public void action(int action) {
                        act(action, row);
                    }
                });
    }

    private void act(int action, Map<String, Object> row) {
        List<Map<String, Object>> one = new ArrayList<>();
        one.add(row);
        if (action == R.string.action_reply) {
            if (Core.str(row, "id").isEmpty()) {
                line(getString(R.string.no_id_yet));
            } else {
                replyTo(row);
            }
        } else if (action == R.string.action_forward) {
            forward(row);
        } else if (action == R.string.action_copy) {
            copy(one);
        } else if (action == R.string.action_select) {
            select(new LinkedHashSet<Long>());
            toggle(row);
        } else if (action == R.string.action_info) {
            info(row);
        } else if (action == R.string.action_pin) {
            pin(Core.num(row, "key"));
        } else if (action == R.string.action_unpin) {
            pin(0);
        } else if (action == R.string.action_delete) {
            askDelete(one);
        } else if (action == R.string.action_run) {
            showRun(row);
        }
    }

    /**
     * Every report the run's row folds (1184), newest first: when, where,
     * how exact; a tap opens the map on that one, as the row does on the
     * newest.
     */
    private void showRun(Map<String, Object> row) {
        final List<Map<String, Object>> run = Core.run(peer, Core.num(row, "key"));
        java.text.DateFormat time = android.text.format.DateFormat.getTimeFormat(this);
        String[] lines = new String[run.size()];
        for (int i = 0; i < run.size(); i++) {
            Map<String, Object> one = run.get(i);
            StringBuilder line = new StringBuilder(
                    time.format(new java.util.Date(Core.num(one, "at") * 1000)));
            if (one.get("place") != null) {
                line.append("  \uD83D\uDCCD ").append(Core.str(one, "place"));
            }
            String accuracy = Core.str(one, "accuracy");
            if (!accuracy.isEmpty()) {
                line.append(" \u00B7 ").append(accuracy);
            }
            lines[i] = line.toString();
        }
        new AlertDialog.Builder(this)
                .setTitle(getString(R.string.run_title, run.size()))
                .setItems(lines, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        showFix(run.get(which));
                    }
                })
                .show();
    }

    /** Write a reply to `row`, its quote above the field; null ends it. */
    private void replyTo(Map<String, Object> row) {
        replyTo = row;
        replyBar.setVisibility(row == null ? View.GONE : View.VISIBLE);
        // The reply is part of the draft (1219), started or ended.
        tellDraft();
        if (row == null) {
            return;
        }
        ((TextView) findViewById(R.id.reply_who)).setText(Core.bool(row, "ours")
                ? getString(R.string.reply_to_ours)
                : getString(R.string.reply_to_theirs, title.getText()));
        ((TextView) findViewById(R.id.reply_text)).setText(Core.str(row, "body"));
        compose.requestFocus();
    }

    /** Pick a conversation and send it a copy of `row`. */
    private void forward(final Map<String, Object> row) {
        final List<Map<String, Object>> all = Core.list(Core.conversations(null), "conversations");
        String[] names = new String[all.size()];
        for (int i = 0; i < all.size(); i++) {
            names[i] = Core.str(all.get(i), "name");
        }
        new AlertDialog.Builder(this)
                .setTitle(R.string.forward_to)
                .setItems(names, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        Map<String, Object> to = all.get(which);
                        String error = Core.forward(peer, Core.num(row, "key"), Core.str(to, "peer"));
                        line(error != null ? error : getString(R.string.forwarded, Core.str(to, "name")));
                    }
                })
                .show();
    }

    /** The texts of `picked` to the clipboard, one after the other. */
    private void copy(List<Map<String, Object>> picked) {
        StringBuilder text = new StringBuilder();
        for (Map<String, Object> row : picked) {
            if (text.length() > 0) {
                text.append("\n\n");
            }
            text.append(Core.str(row, "body"));
        }
        ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
        if (clipboard == null) {
            return;
        }
        clipboard.setPrimaryClip(ClipData.newPlainText(getString(R.string.app_name), text));
        // Android 13 and later say so themselves.
        if (Build.VERSION.SDK_INT < 33) {
            line(getString(R.string.copied));
        }
    }

    /** Start selecting with `keys`, or end it with null. */
    private void select(Set<Long> keys) {
        selection = keys;
        selectionBar.setVisibility(keys == null ? View.GONE : View.VISIBLE);
        adapter.notifyDataSetChanged();
    }

    private void toggle(Map<String, Object> row) {
        Long key = Core.num(row, "key");
        if (!selection.remove(key)) {
            selection.add(key);
        }
        if (selection.isEmpty()) {
            select(null);
            return;
        }
        selectionCount.setText(getString(R.string.selected, selection.size()));
        adapter.notifyDataSetChanged();
    }

    /** The selected rows, in the conversation's order. */
    private List<Map<String, Object>> selected() {
        List<Map<String, Object>> picked = new ArrayList<>();
        for (Map<String, Object> row : rows) {
            if (selection != null && selection.contains(Core.num(row, "key"))) {
                picked.add(row);
            }
        }
        return picked;
    }

    /** Ask, then delete `picked`: from every device of the home on a client of one. */
    private void askDelete(final List<Map<String, Object>> picked) {
        if (picked.isEmpty()) {
            return;
        }
        int n = picked.size();
        String question = n == 1
                ? getString(homeClient ? R.string.delete_one_home : R.string.delete_one_here)
                : getString(homeClient ? R.string.delete_many_home : R.string.delete_many_here, n);
        new AlertDialog.Builder(this)
                .setMessage(question)
                .setPositiveButton(R.string.delete, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        List<Long> keys = new ArrayList<>();
                        for (Map<String, Object> row : picked) {
                            keys.add(Core.num(row, "key"));
                        }
                        say(Core.delete(peer, keys));
                        if (selection != null) {
                            select(null);
                        }
                        refresh();
                    }
                })
                .setNegativeButton(R.string.keep, null)
                .show();
    }

    /** The message's detail, the terminal's `v` view, in a dialog. */
    private void info(Map<String, Object> row) {
        String[] error = new String[1];
        List<String> lines = Core.info(peer, Core.num(row, "key"), error);
        if (lines == null) {
            line(error[0]);
            return;
        }
        StringBuilder text = new StringBuilder();
        for (String l : lines) {
            text.append(l).append('\n');
        }
        TextView view = new TextView(this);
        view.setText(text.toString().trim());
        view.setTypeface(Typeface.MONOSPACE);
        view.setTextSize(13);
        view.setTextIsSelectable(true);
        view.setTextColor(getColor(R.color.text));
        view.setPadding(dp(20), dp(12), dp(20), dp(12));
        ScrollView scroll = new ScrollView(this);
        scroll.addView(view);
        new AlertDialog.Builder(this)
                .setTitle(R.string.info)
                .setView(scroll)
                .setPositiveButton(android.R.string.ok, null)
                .show();
    }

    private SharedPreferences pins() {
        return getSharedPreferences(PINS, MODE_PRIVATE);
    }

    /**
     * The key of this conversation's pinned message, 0 for none: a key of either sign is a row's,
     * but 0 is seq i64::MIN's (Rust's schema::wire_key), which no store reaches.
     */
    private long pinnedKey() {
        return pins().getLong(peer, 0);
    }

    /** Pin the message `key`, or none with 0; one per conversation. */
    private void pin(long key) {
        if (key == 0) {
            pins().edit().remove(peer).apply();
        } else {
            pins().edit().putLong(peer, key).apply();
        }
        showPinned();
    }

    /** The pinned bar: the pinned message's text, while it is still here. */
    private void showPinned() {
        long key = pinnedKey();
        Map<String, Object> row = null;
        for (Map<String, Object> r : rows) {
            if (key != 0 && Core.num(r, "key") == key) {
                row = r;
            }
        }
        pinned.setVisibility(row == null ? View.GONE : View.VISIBLE);
        if (row != null) {
            String body = Core.str(row, "body");
            pinned.setText("\uD83D\uDCCC " + (body.isEmpty() ? Core.str(row, "place") : body));
            pinned.setContentDescription(getString(R.string.pinned) + ": " + body);
        }
    }

    private void goToPinned() {
        long key = pinnedKey();
        for (int i = 0; i < rows.size(); i++) {
            if (Core.num(rows.get(i), "key") == key) {
                list.setSelection(i);
                return;
            }
        }
    }

    private void refresh() {
        Map<String, Object> answer = Core.messages(peer);
        if (answer.containsKey("error")) {
            line(Core.str(answer, "error"));
            return;
        }
        header = answer;
        // The verified mark after the name (1146), once the identity was compared.
        title.setText(Core.str(answer, "name")
                + (Core.bool(answer, "verified") ? " \u2714" : ""));
        avatar.show(peer, answer);
        banner.setVisibility(Core.bool(answer, "ask_link") ? View.VISIBLE : View.GONE);
        homeClient = Core.bool(answer, "home_client");
        rows.clear();
        rows.addAll(Core.list(answer, "messages"));
        if (selection != null) {
            // What another device deleted is no longer selected.
            Set<Long> here = new LinkedHashSet<>();
            for (Map<String, Object> row : rows) {
                here.add(Core.num(row, "key"));
            }
            selection.retainAll(here);
            if (selection.isEmpty()) {
                select(null);
            } else {
                selectionCount.setText(getString(R.string.selected, selection.size()));
            }
        }
        showPinned();
        adapter.notifyDataSetChanged();
        if (draftOwed) {
            draftOwed = false;
            mirrorDraft(answer);
        }
    }

    /** The field and the reply bar as the library's draft has them (1219). */
    private void mirrorDraft(Map<String, Object> answer) {
        mirroring = true;
        try {
            String draft = Core.str(answer, "draft");
            if (!draft.equals(compose.getText().toString())) {
                compose.setText(draft);
                compose.setSelection(compose.getText().length());
            }
            String reply = Core.str(answer, "draft_reply");
            Map<String, Object> target = null;
            for (Map<String, Object> row : rows) {
                if (!reply.isEmpty() && reply.equals(Core.str(row, "id"))) {
                    target = row;
                }
            }
            replyTo(target);
        } finally {
            mirroring = false;
        }
    }

    /** What the field holds, and the reply, to the library as the draft. */
    private void tellDraft() {
        if (mirroring || peer == null) {
            return;
        }
        Core.draft(peer, compose.getText().toString(),
                replyTo == null ? "" : Core.str(replyTo, "id"));
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        if ("changed".equals(Core.str(event, "t"))) {
            refresh();
            return;
        }
        if ("stopped".equals(Core.str(event, "t"))) {
            finish();
            return;
        }
        String words = Ui.eventLine(this, event);
        if (words != null) {
            Ui.showLine(status, words, Ui.stays(event));
        }
    }

    private final class Adapter extends BaseAdapter {
        @Override
        public int getCount() {
            return rows.size();
        }

        @Override
        public Object getItem(int position) {
            return rows.get(position);
        }

        @Override
        public long getItemId(int position) {
            return position;
        }

        @Override
        public View getView(int position, View view, ViewGroup parent) {
            if (view == null) {
                view = getLayoutInflater().inflate(R.layout.row_message, parent, false);
            }
            Map<String, Object> row = rows.get(position);
            boolean ours = Core.bool(row, "ours");
            // Where the bubble sits is the library's (1217): its group,
            // its day line, whether it shows its time and mark.
            boolean joinsBefore = Core.bool(row, "joins_before");
            boolean joinsAfter = Core.bool(row, "joins_after");
            long at = Core.num(row, "at");
            long now = Ui.nowSecs();
            LinearLayout root = (LinearLayout) view;
            root.setGravity(ours ? Gravity.END : Gravity.START);
            boolean picked = selection != null && selection.contains(Core.num(row, "key"));
            boolean found = Long.valueOf(Core.num(row, "key")).equals(searchHit);
            root.setBackgroundColor(picked || found ? getColor(R.color.selected_row) : 0);

            TextView day = (TextView) view.findViewById(R.id.day);
            String dayWords = Core.str(row, "day");
            boolean begins = !dayWords.isEmpty();
            day.setVisibility(begins ? View.VISIBLE : View.GONE);
            day.setText(dayWords);
            root.setPadding(root.getPaddingStart(), joinsBefore ? dp(2) : dp(begins ? 0 : 10),
                    root.getPaddingEnd(), 0);

            BubbleLayout bubble = (BubbleLayout) view.findViewById(R.id.bubble);
            int width = parent.getWidth() > 0
                    ? parent.getWidth() : getResources().getDisplayMetrics().widthPixels;
            bubble.setMaxWidth(Math.round(width * 0.8f));
            float big = dp(CORNER);
            float small = dp(CORNER_GROUPED);
            float top = joinsBefore ? small : big;
            float bottom = joinsAfter ? small : big;
            GradientDrawable shape = new GradientDrawable();
            shape.setColor(getColor(ours ? R.color.bubble_ours : R.color.bubble_theirs));
            // Top left, top right, bottom right, bottom left; the inner
            // side is the sender's: the right for ours.
            shape.setCornerRadii(ours
                    ? new float[] {big, big, top, top, bottom, bottom, big, big}
                    : new float[] {top, top, big, big, big, big, bottom, bottom});
            bubble.setBackground(shape);

            photo((ImageView) view.findViewById(R.id.photo), row);
            files((LinearLayout) view.findViewById(R.id.files), row, ours);
            voice((LinearLayout) view.findViewById(R.id.voice), row, ours);

            // The words under the media, none where the scene has none
            // (1268): a photo alone is the photo and its time in one bubble.
            TextView body = (TextView) view.findViewById(R.id.body);
            CharSequence words = text(row, ours, now);
            body.setText(words);
            body.setVisibility(words.length() == 0 ? View.GONE : View.VISIBLE);
            body.setTextColor(getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text));
            quote(view, row, ours);

            // Only a group's last shows its time, and one of ours whose
            // state differs from the next one's keeps its mark.
            String state = Core.str(row, "state");
            boolean showMeta = Core.bool(row, "meta");
            View meta = view.findViewById(R.id.meta);
            meta.setVisibility(showMeta && (at > 0 || ours) ? View.VISIBLE : View.GONE);
            int metaColor = getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
            TextView time = (TextView) view.findViewById(R.id.time);
            String ago = Core.bool(row, "newest") ? Ui.ago(ChatActivity.this, at, now) : null;
            time.setText(at <= 0 ? "" : ago != null ? ago : Ui.clock(at));
            time.setVisibility(at > 0 ? View.VISIBLE : View.GONE);
            time.setTextColor(metaColor);
            // Found by its tag: its id is the rung's (1232), which the
            // device walk reads.
            ImageView mark = (ImageView) view.findViewWithTag("mark");
            mark.setVisibility(ours ? View.VISIBLE : View.GONE);
            mark.setOnClickListener(null);
            mark.setClickable(false);
            if (ours) {
                Glyph.Kind kind = Glyph.mark(state);
                int ink = kind == Glyph.Kind.TICKS_READ ? getColor(R.color.read_mark) : metaColor;
                mark.setImageDrawable(new Glyph(kind, ink,
                        getColor(R.color.failed_ground), getColor(R.color.failed_mark)));
                mark.setId(Glyph.resource(kind));
                mark.setContentDescription(Ui.state(ChatActivity.this, state, Core.num(row, "read_at")));
                if (kind == Glyph.Kind.FAILED) {
                    final long key = Core.num(row, "key");
                    mark.setOnClickListener(new View.OnClickListener() {
                        @Override
                        public void onClick(View v) {
                            String error = Core.sendAgain(peer, key);
                            if (error != null) {
                                line(error);
                            }
                        }
                    });
                }
            }

            LinearLayout chips = (LinearLayout) view.findViewById(R.id.chips);
            chips.removeAllViews();
            for (Map<String, Object> chip : Core.list(row, "chips")) {
                chips.addView(pill(chip));
            }
            chips.setVisibility(chips.getChildCount() > 0 ? View.VISIBLE : View.GONE);
            return view;
        }

        /**
         * A reply's quote above the body: a stripe, who wrote the message
         * answered, its first line; in the bubble's meta colour.
         */
        private void quote(View view, Map<String, Object> row, boolean ours) {
            View quote = view.findViewById(R.id.quote);
            boolean has = row.get("quote") != null;
            quote.setVisibility(has ? View.VISIBLE : View.GONE);
            if (!has) {
                return;
            }
            int meta = getColor(ours ? R.color.bubble_ours_meta : R.color.bubble_theirs_meta);
            view.findViewById(R.id.quote_stripe).setBackgroundColor(meta);
            TextView who = (TextView) view.findViewById(R.id.quote_who);
            Object quoteOurs = row.get("quote_ours");
            who.setVisibility(quoteOurs == null ? View.GONE : View.VISIBLE);
            who.setText(Boolean.TRUE.equals(quoteOurs) ? getString(R.string.quote_you) : title.getText());
            who.setTextColor(meta);
            TextView text = (TextView) view.findViewById(R.id.quote_text);
            text.setText(Core.str(row, "quote"));
            text.setTextColor(meta);
        }

        /** A reaction: its emoji, a count above one, ours in our bubble's colour. */
        private TextView pill(Map<String, Object> chip) {
            boolean ours = Core.bool(chip, "ours");
            TextView pill = new TextView(ChatActivity.this);
            String text = Core.str(chip, "content");
            long count = Core.num(chip, "count");
            pill.setText(count > 1 ? text + " " + count : text);
            pill.setTextSize(14);
            pill.setTextColor(getColor(ours ? R.color.bubble_ours_text : R.color.bubble_theirs_text));
            pill.setPadding(dp(8), dp(2), dp(8), dp(2));
            GradientDrawable shape = new GradientDrawable();
            shape.setCornerRadius(dp(14));
            shape.setColor(getColor(ours ? R.color.bubble_ours : R.color.bubble_theirs));
            // A ring in the screen's colour lifts the pill off the bubble.
            shape.setStroke(dp(2), getColor(R.color.background));
            pill.setBackground(shape);
            LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            params.setMarginEnd(dp(2));
            pill.setLayoutParams(params);
            return pill;
        }
    }
}
