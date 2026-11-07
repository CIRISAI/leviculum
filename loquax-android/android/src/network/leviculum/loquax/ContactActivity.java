package network.leviculum.loquax;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.DialogInterface;
import android.content.Intent;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.text.format.DateFormat;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * The contact screen (1146), after Signal's, which a tap on the chat's
 * name or avatar opens, and the chat menu's "Contact info": the avatar
 * and the name, which opens the linked contact; round buttons to mute,
 * search the conversation, open the map at the newest position and share
 * the address; rows for the nickname, the notifications and the phone
 * contact; Advanced, collapsed (1283, walk finding 29): the identity to
 * compare in person and what this phone knows of reaching the peer; its
 * positions; block and delete at the bottom.
 *
 * Pin, mute, archive and block reach every device of the home through the
 * home node; verified stays on this phone. What the chat itself has to do
 * (search, link a contact, close) goes back to it as the result.
 */
public class ContactActivity extends Activity implements Events.Listener {
    static final String PEER = "peer";
    /** The result's extra: what the chat does next. */
    static final String THEN = "then";
    static final String THEN_SEARCH = "search";
    static final String THEN_LINK = "link";
    static final String THEN_CLOSE = "close";
    /** The mute choices, in seconds; negative is always. */
    private static final long[] MUTES = {3600, 8 * 3600, 7 * 24 * 3600, -1};

    private String peer;
    private Map<String, Object> detail = Core.map();
    private LinearLayout body;
    /** Whether Advanced is open, kept across a refresh. */
    private boolean advancedOpen;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        peer = getIntent().getStringExtra(PEER);
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
        screen.addView(bar, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(56)));
        ScrollView scroll = new ScrollView(this);
        scroll.setId(R.id.contact_scroll);
        body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(0, 0, 0, dp(32));
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
        Events.listen(this);
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.front(true);
            }
        });
        refresh();
    }

    @Override
    protected void onPause() {
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.front(false);
            }
        });
        Events.listen(null);
        super.onPause();
    }

    @Override
    public void onEvent(Map<String, Object> event) {
        String kind = Core.str(event, "t");
        if ("changed".equals(kind)) {
            refresh();
        } else if ("stopped".equals(kind)) {
            finish();
        }
    }

    /** A question for the screen is with the engine, and another is owed after it. */
    private boolean asking;
    private boolean askOwed;

    /**
     * The contact again, asked on the engine's thread (1464): one question
     * at a time, and the events that came while it was out make one more.
     */
    private void refresh() {
        if (asking) {
            askOwed = true;
            return;
        }
        asking = true;
        final String of = peer;
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.detail(of);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                asking = false;
                if (isDestroyed() || isFinishing()) {
                    return;
                }
                show(answer);
                if (askOwed) {
                    askOwed = false;
                    refresh();
                }
            }
        });
    }

    private void show(Map<String, Object> answer) {
        if (answer.containsKey("error")) {
            Toast.makeText(this, Core.str(answer, "error"), Toast.LENGTH_LONG).show();
            finish();
            return;
        }
        detail = answer;
        body.removeAllViews();
        head();
        actions();
        settings();
        advanced();
        positions();
        bottom();
    }

    /** The avatar, the name with its ">" to the linked contact, the verified mark. */
    private void head() {
        AvatarView avatar = new AvatarView(this, null);
        avatar.show(peer, detail);
        LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(dp(96), dp(96));
        at.gravity = Gravity.CENTER_HORIZONTAL;
        at.topMargin = dp(8);
        body.addView(avatar, at);
        TextView name = text(Core.str(detail, "name") + "  ›", 24, R.color.text);
        name.setGravity(Gravity.CENTER);
        name.setPadding(dp(16), dp(12), dp(16), 0);
        name.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                phoneContact();
            }
        });
        body.addView(name, matchWrap());
        if (Core.bool(detail, "verified")) {
            TextView mark = text("✔ " + getString(R.string.verified), 14, R.color.accent);
            mark.setGravity(Gravity.CENTER);
            body.addView(mark, matchWrap());
        }
    }

    /** Mute, search, the map when there are positions, share. */
    private void actions() {
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_HORIZONTAL);
        row.setPadding(0, dp(20), 0, dp(12));
        boolean muted = Core.bool(detail, "muted");
        row.addView(action(muted ? "🔕" : "🔔",
                getString(muted ? R.string.muted_mark : R.string.mute), new Runnable() {
                    @Override
                    public void run() {
                        mute();
                    }
                }));
        row.addView(action("🔍", getString(R.string.chat_search), new Runnable() {
            @Override
            public void run() {
                back(THEN_SEARCH);
            }
        }));
        final List<Map<String, Object>> fixes = Core.list(detail, "positions");
        if (!fixes.isEmpty()) {
            row.addView(action("🗺️", getString(R.string.map), new Runnable() {
                @Override
                public void run() {
                    map(fixes.get(0));
                }
            }));
        }
        row.addView(action("↗", getString(R.string.share), new Runnable() {
            @Override
            public void run() {
                share();
            }
        }));
        body.addView(row, matchWrap());
    }

    /** Nickname, notifications, phone contact. */
    private void settings() {
        String petname = Core.str(detail, "petname");
        setting("✏️", getString(R.string.nickname),
                petname.isEmpty() ? getString(R.string.nickname_none) : petname, new Runnable() {
                    @Override
                    public void run() {
                        rename();
                    }
                });
        setting("🔔", getString(R.string.sounds), muteLine(), new Runnable() {
            @Override
            public void run() {
                mute();
            }
        });
        setting("📇", getString(R.string.phone_contact),
                Core.bool(detail, "linked") ? Core.str(detail, "name")
                        : getString(R.string.phone_contact_none), new Runnable() {
                    @Override
                    public void run() {
                        phoneContact();
                    }
                });
    }

    /** Advanced, collapsed: the identity and the network, which need Reticulum words. */
    private void advanced() {
        final TextView header = text(getString(R.string.advanced) + (advancedOpen ? "  ▴" : "  ▾"),
                16, R.color.text);
        header.setId(R.id.contact_advanced);
        header.setPadding(dp(16), dp(24), dp(16), dp(12));
        header.setBackgroundResource(android.R.drawable.list_selector_background);
        body.addView(header, matchWrap());
        final LinearLayout fold = new LinearLayout(this);
        fold.setId(R.id.contact_advanced_body);
        fold.setOrientation(LinearLayout.VERTICAL);
        fold.setVisibility(advancedOpen ? View.VISIBLE : View.GONE);
        body.addView(fold, matchWrap());
        header.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                advancedOpen = !advancedOpen;
                fold.setVisibility(advancedOpen ? View.VISIBLE : View.GONE);
                header.setText(getString(R.string.advanced) + (advancedOpen ? "  ▴" : "  ▾"));
            }
        });
        // The rows add themselves to `body`: point it at the fold meanwhile.
        LinearLayout outer = body;
        body = fold;
        try {
            identity();
            network();
        } finally {
            body = outer;
        }
    }

    /** The address in groups, its QR code, why to compare, and the verified switch. */
    private void identity() {
        header(getString(R.string.identity));
        TextView fingerprint = text(Core.str(detail, "fingerprint"), 18, R.color.text);
        fingerprint.setTypeface(Typeface.MONOSPACE);
        fingerprint.setGravity(Gravity.CENTER);
        fingerprint.setPadding(dp(16), dp(4), dp(16), dp(8));
        fingerprint.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                ClipboardManager clip = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
                clip.setPrimaryClip(ClipData.newPlainText("LXMF", peer));
                Toast.makeText(ContactActivity.this, R.string.address_copied, Toast.LENGTH_SHORT).show();
            }
        });
        body.addView(fingerprint, matchWrap());
        ImageView qr = qr(dp(200));
        if (qr != null) {
            LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(dp(200), dp(200));
            at.gravity = Gravity.CENTER_HORIZONTAL;
            body.addView(qr, at);
        }
        TextView why = text(getString(R.string.identity_why), 13, R.color.text_secondary);
        why.setPadding(dp(16), dp(8), dp(16), dp(4));
        body.addView(why, matchWrap());
        final boolean verified = Core.bool(detail, "verified");
        setting(verified ? "✔" : "☐",
                getString(verified ? R.string.clear_verified : R.string.mark_verified), null,
                new Runnable() {
                    @Override
                    public void run() {
                        final String to = peer;
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return Core.verify(to, !verified);
                            }
                        }, new Engine.Said() {
                            @Override
                            public void said(String error) {
                                refresh();
                            }
                        });
                    }
                });
    }

    /** What this phone knows of reaching the peer; a client has no path of its own. */
    private void network() {
        header(getString(R.string.network));
        long now = Ui.nowSecs();
        long received = Core.num(detail, "last_received");
        fact(getString(R.string.last_from_them),
                received == 0 ? getString(R.string.never) : Ui.ago(this, received, now));
        long sent = Core.num(detail, "last_sent");
        String how = Core.str(detail, "last_sent_how");
        String to = sent == 0 && how.isEmpty() ? getString(R.string.never)
                : (sent == 0 ? how : Ui.ago(this, sent, now) + (how.isEmpty() ? "" : " · " + how));
        fact(getString(R.string.last_to_them), to);
        if (Core.bool(detail, "home_client")) {
            fact(getString(R.string.path), getString(R.string.path_home));
        }
    }

    /** In Signal's "shared media" place: the newest positions, each opening the map. */
    private void positions() {
        List<Map<String, Object>> fixes = Core.list(detail, "positions");
        if (fixes.isEmpty()) {
            return;
        }
        header(getString(R.string.positions));
        long now = Ui.nowSecs();
        for (final Map<String, Object> fix : fixes) {
            long at = Core.num(fix, "at");
            String place = String.format(Locale.ROOT, "%.5f, %.5f",
                    Core.num(fix, "lat_e6") / 1e6, Core.num(fix, "lon_e6") / 1e6);
            setting("📍", Core.str(fix, "day") + " " + Ui.clock(at),
                    place + " · " + Ui.age(this, at, now), new Runnable() {
                        @Override
                        public void run() {
                            map(fix);
                        }
                    });
        }
    }

    /** Block (in Signal's "report spam" place) and delete. */
    private void bottom() {
        body.addView(new View(this), new LinearLayout.LayoutParams(1, dp(16)));
        final boolean blocked = Core.bool(detail, "blocked");
        TextView block = danger(getString(blocked ? R.string.unblock : R.string.block));
        block.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                if (blocked) {
                    final String to = peer;
                    Engine.run(new Engine.Call() {
                        @Override
                        public String run() {
                            return Core.block(to, false);
                        }
                    }, new Engine.Said() {
                        @Override
                        public void said(String error) {
                            refresh();
                        }
                    });
                } else {
                    askBlock();
                }
            }
        });
        body.addView(block, matchWrap());
        TextView delete = danger(getString(R.string.delete_conversation));
        delete.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                askDelete();
            }
        });
        body.addView(delete, matchWrap());
    }

    private void askBlock() {
        final String name = Core.str(detail, "name");
        new AlertDialog.Builder(this)
                .setMessage(getString(R.string.block_ask, name))
                .setPositiveButton(R.string.block, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        final String to = peer;
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return Core.block(to, true);
                            }
                        }, new Engine.Said() {
                            @Override
                            public void said(String error) {
                                Toast.makeText(ContactActivity.this,
                                        error != null ? error : getString(R.string.blocked_done, name),
                                        Toast.LENGTH_SHORT).show();
                                back(THEN_CLOSE);
                            }
                        });
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    private void askDelete() {
        deleteDialog(this, peer, Core.str(detail, "name"), Core.bool(detail, "home_client"),
                new Runnable() {
                    @Override
                    public void run() {
                        back(THEN_CLOSE);
                    }
                });
    }

    /**
     * Ask whether to delete `peer`'s conversation here or on every device
     * of the home (only a home client has a choice), delete it, run `done`.
     */
    static void deleteDialog(final Activity activity, final String peer, String name,
            boolean homeClient, final Runnable done) {
        AlertDialog.Builder builder = new AlertDialog.Builder(activity)
                .setTitle(activity.getString(R.string.delete_conversation_ask, name))
                .setNegativeButton(android.R.string.cancel, null);
        DialogInterface.OnClickListener here = new DialogInterface.OnClickListener() {
            @Override
            public void onClick(DialogInterface d, int which) {
                Engine.run(new Engine.Call() {
                    @Override
                    public String run() {
                        return Core.forget(peer, false);
                    }
                }, new Engine.Said() {
                    @Override
                    public void said(String error) {
                        done.run();
                    }
                });
            }
        };
        if (homeClient) {
            builder.setNeutralButton(R.string.delete_here, here)
                    .setPositiveButton(R.string.delete_home, new DialogInterface.OnClickListener() {
                        @Override
                        public void onClick(DialogInterface d, int which) {
                            Engine.run(new Engine.Call() {
                                @Override
                                public String run() {
                                    return Core.forget(peer, true);
                                }
                            }, new Engine.Said() {
                                @Override
                                public void said(String error) {
                                    done.run();
                                }
                            });
                        }
                    });
        } else {
            builder.setPositiveButton(R.string.delete, here);
        }
        builder.show();
    }

    /** The mute's durations, or unmute while muted. */
    private void mute() {
        muteDialog(this, peer, Core.bool(detail, "muted"), new Runnable() {
            @Override
            public void run() {
                refresh();
            }
        });
    }

    /** Signal's mute choices for `peer`: 1 h, 8 h, a week, always; or unmute. */
    static void muteDialog(final Activity activity, final String peer, boolean muted,
            final Runnable done) {
        if (muted) {
            Engine.run(new Engine.Call() {
                @Override
                public String run() {
                    return Core.mute(peer, 0);
                }
            }, new Engine.Said() {
                @Override
                public void said(String error) {
                    done.run();
                }
            });
            return;
        }
        String[] choices = {
            activity.getString(R.string.mute_hour), activity.getString(R.string.mute_8h),
            activity.getString(R.string.mute_week), activity.getString(R.string.mute_always),
        };
        new AlertDialog.Builder(activity)
                .setTitle(R.string.mute_for)
                .setItems(choices, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        final long until = MUTES[which];
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return Core.mute(peer, until);
                            }
                        }, new Engine.Said() {
                            @Override
                            public void said(String error) {
                                done.run();
                            }
                        });
                    }
                })
                .show();
    }

    private String muteLine() {
        if (!Core.bool(detail, "muted")) {
            return getString(R.string.notifications_on);
        }
        long until = Core.num(detail, "muted_until");
        if (until < 0) {
            return getString(R.string.muted_always);
        }
        String when = DateFormat.getMediumDateFormat(this).format(until * 1000) + " "
                + Ui.clock(until);
        return getString(R.string.muted_until, when);
    }

    /** The user's own name for the peer, for every device of the home. */
    private void rename() {
        final EditText field = new EditText(this);
        field.setSingleLine(true);
        field.setText(Core.str(detail, "petname"));
        field.setSelection(field.getText().length());
        field.setHint(R.string.rename_hint);
        LinearLayout frame = new LinearLayout(this);
        frame.setPadding(dp(20), dp(8), dp(20), 0);
        frame.addView(field, matchWrap());
        new AlertDialog.Builder(this)
                .setTitle(R.string.nickname)
                .setView(frame)
                .setPositiveButton(android.R.string.ok, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        final String to = peer;
                        final String name = field.getText().toString();
                        Engine.run(new Engine.Call() {
                            @Override
                            public String run() {
                                return Core.rename(to, name);
                            }
                        }, new Engine.Said() {
                            @Override
                            public void said(String error) {
                                refresh();
                            }
                        });
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    /** The linked contact's card, or back to the chat to link one (1139). */
    private void phoneContact() {
        if (!Core.bool(detail, "linked")) {
            back(THEN_LINK);
            return;
        }
        if (!Book.open(this, Core.num(detail, "contact_id"), Core.str(detail, "lookup"))) {
            Toast.makeText(this, R.string.no_contacts_app, Toast.LENGTH_SHORT).show();
        }
    }

    /** The address to show: its QR code large, and to share as text. */
    private void share() {
        LinearLayout frame = new LinearLayout(this);
        frame.setOrientation(LinearLayout.VERTICAL);
        frame.setGravity(Gravity.CENTER_HORIZONTAL);
        frame.setPadding(dp(16), dp(16), dp(16), 0);
        ImageView qr = qr(dp(260));
        if (qr != null) {
            frame.addView(qr, new LinearLayout.LayoutParams(dp(260), dp(260)));
        }
        TextView address = text(Core.str(detail, "fingerprint"), 16, R.color.text);
        address.setTypeface(Typeface.MONOSPACE);
        address.setGravity(Gravity.CENTER);
        address.setPadding(0, dp(12), 0, 0);
        frame.addView(address, matchWrap());
        new AlertDialog.Builder(this)
                .setTitle(Core.str(detail, "name"))
                .setView(frame)
                .setPositiveButton(R.string.share_text, new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        Intent send = new Intent(Intent.ACTION_SEND)
                                .setType("text/plain")
                                .putExtra(Intent.EXTRA_TEXT, peer);
                        startActivity(Intent.createChooser(send, getString(R.string.share)));
                    }
                })
                .setNegativeButton(android.R.string.cancel, null)
                .show();
    }

    private void map(Map<String, Object> fix) {
        startActivity(new Intent(this, MapActivity.class)
                .putExtra(MapActivity.LAT_E6, Core.num(fix, "lat_e6"))
                .putExtra(MapActivity.LON_E6, Core.num(fix, "lon_e6"))
                .putExtra(MapActivity.SELECTED, peer));
    }

    /** Back to the chat with what it does next. */
    private void back(String then) {
        setResult(RESULT_OK, new Intent().putExtra(THEN, then));
        finish();
    }

    /** The address's QR code as a picture `side` pixels wide, black on white with its quiet zone. */
    private ImageView qr(int side) {
        return QrCode.view(this, detail, side, Core.str(detail, "fingerprint"));
    }

    private View action(String icon, String label, final Runnable run) {
        LinearLayout column = new LinearLayout(this);
        column.setOrientation(LinearLayout.VERTICAL);
        column.setGravity(Gravity.CENTER_HORIZONTAL);
        column.setPadding(dp(10), 0, dp(10), 0);
        TextView circle = text(icon, 22, R.color.text);
        circle.setGravity(Gravity.CENTER);
        GradientDrawable ground = new GradientDrawable();
        ground.setShape(GradientDrawable.OVAL);
        ground.setColor(getColor(R.color.bar));
        circle.setBackground(ground);
        column.addView(circle, new LinearLayout.LayoutParams(dp(56), dp(56)));
        TextView words = text(label, 12, R.color.text_secondary);
        words.setGravity(Gravity.CENTER);
        words.setPadding(0, dp(6), 0, 0);
        column.addView(words, new LinearLayout.LayoutParams(dp(76), ViewGroup.LayoutParams.WRAP_CONTENT));
        column.setContentDescription(label);
        column.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                run.run();
            }
        });
        return column;
    }

    /** One row: an icon, its title, and under it its value when it has one. */
    private void setting(String icon, String title, String value, final Runnable run) {
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(12), dp(16), dp(12));
        row.setBackgroundResource(android.R.drawable.list_selector_background);
        TextView glyph = text(icon, 20, R.color.text);
        glyph.setGravity(Gravity.CENTER);
        row.addView(glyph, new LinearLayout.LayoutParams(dp(40), ViewGroup.LayoutParams.WRAP_CONTENT));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        words.setPadding(dp(16), 0, 0, 0);
        words.addView(text(title, 16, R.color.text), matchWrap());
        if (value != null && !value.isEmpty()) {
            words.addView(text(value, 14, R.color.text_secondary), matchWrap());
        }
        row.addView(words, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        if (run != null) {
            row.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    run.run();
                }
            });
        }
        body.addView(row, matchWrap());
    }

    private void fact(String title, String value) {
        setting("", title, value, null);
    }

    private void header(String words) {
        TextView header = text(words, 14, R.color.accent);
        header.setPadding(dp(16), dp(20), dp(16), dp(4));
        body.addView(header, matchWrap());
    }

    private TextView danger(String words) {
        TextView view = text(words, 16, R.color.failed_mark_on_panel);
        view.setPadding(dp(72), dp(14), dp(16), dp(14));
        view.setBackgroundResource(android.R.drawable.list_selector_background);
        return view;
    }

    private TextView text(String words, float sp, int colour) {
        TextView view = new TextView(this);
        view.setText(words);
        view.setTextSize(sp);
        view.setTextColor(getColor(colour));
        return view;
    }

    private static LinearLayout.LayoutParams matchWrap() {
        return new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}
