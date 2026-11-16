package network.leviculum.loquax;

import android.app.Activity;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;
import java.util.Map;

/**
 * The first screen: starts the session, and on a phone that is not joined
 * yet asks for the home code and the home node's address. Once started it
 * hands over to the conversation list.
 *
 * A code from {@code loquax mailbox admit --server} carries the address
 * ({@code lqxh1...@host:port}): pasted, it fills the address field too;
 * shared from another app (a QR reader's share button), it fills both and
 * joins at once.
 *
 * A phone that neither holds a mailbox nor reads one goes to the first
 * start ({@link WelcomeActivity}) instead, unless it brings a code or asks
 * for the form.
 *
 * The IM row of protocol LXMF on a contact's card (1139) taps into
 * {@code imto://lxmf/<address>}: the session starts as ever, and the
 * address's conversation opens over the list.
 */
public class MainActivity extends Activity {
    /** The extra that asks for the join form: the first start's "Paste a code". */
    static final String JOIN = "join";
    /** The extra with a code the scanner read: joined at once when it carries the address. */
    static final String CODE = "code";
    private TextView error;
    private View form;
    private EditText code;
    private EditText server;
    private Button join;
    private Button retry;
    private TextView detailsToggle;
    private TextView details;
    /** "Advanced" and the address field under it (1448, JoinForm). */
    private TextView advanced;
    private View serverBox;
    /** The user opened Advanced. */
    private boolean advancedOpened;
    /** What the last failed start needed: "code", "server", or another word. */
    private String lastNeed = "";
    /** The running start is a join the user asked for, not the screen's own look (1448). */
    private boolean asked;
    /** The conversation an imto tap asked for, opened once started. */
    private String imto;
    /** A friend's code from outside (walk finding 12), added once started. */
    private String contact;
    /** This phone's mailbox moved to a new phone (1281): the retry button takes it back. */
    private boolean moved;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Intent intent = getIntent();
        // Recreated, the code was added already.
        contact = savedInstanceState == null ? contactCode(intent) : null;
        // A friend's code on a phone that is not set up yet: the first
        // start comes first, the code is not kept.
        if (!WelcomeActivity.settled(this) && !intent.getBooleanExtra(JOIN, false)
                && (!Intent.ACTION_SEND.equals(intent.getAction()) || contactCode(intent) != null)) {
            startActivity(new Intent(this, WelcomeActivity.class));
            finish();
            return;
        }
        setContentView(R.layout.activity_main);
        error = (TextView) findViewById(R.id.error);
        form = findViewById(R.id.form);
        code = (EditText) findViewById(R.id.code);
        server = (EditText) findViewById(R.id.server);
        join = (Button) findViewById(R.id.join);
        retry = (Button) findViewById(R.id.join_retry);
        detailsToggle = (TextView) findViewById(R.id.join_details_toggle);
        details = (TextView) findViewById(R.id.join_details);
        advanced = (TextView) findViewById(R.id.join_advanced);
        serverBox = findViewById(R.id.join_server_box);
        advanced.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                advancedOpened = !advancedOpened;
                showAdvanced();
            }
        });
        retry.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                if (moved) {
                    askTakeBack();
                } else {
                    start(true);
                }
            }
        });
        detailsToggle.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showDetails(details.getVisibility() != View.VISIBLE);
            }
        });
        // No status line above the form (walk finding 20): the version
        // is the profile's, under Advanced.
        form.setVisibility(View.GONE);
        join.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                start(true);
            }
        });
        code.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                fillServer(s.toString());
            }
        });
        // Recreated (a rotation, the phone's dark setting) the shared code
        // already joined or failed once: the fields restore what was typed,
        // and a second join with a code the home node took would fail.
        showAdvanced();
        imto = Book.imtoPeer(getIntent());
        start(savedInstanceState == null && (shared(getIntent()) || scanned(getIntent())));
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        imto = Book.imtoPeer(intent);
        contact = contactCode(intent);
        if (shared(intent)) {
            start(true);
        } else if (imto != null || contact != null) {
            start(false);
        }
    }

    /**
     * A friend's code the intent brings (walk finding 12): an lxma:// link
     * tapped in another app, or the first word of a shared text the code
     * reader takes as a friend's code. A mailbox code is not one: it keeps
     * the join's path. Null for none.
     */
    private static String contactCode(Intent intent) {
        if (intent == null) {
            return null;
        }
        if (Intent.ACTION_VIEW.equals(intent.getAction()) && intent.getData() != null
                && "lxma".equalsIgnoreCase(intent.getData().getScheme())) {
            return intent.getData().toString();
        }
        if (!Intent.ACTION_SEND.equals(intent.getAction())) {
            return null;
        }
        CharSequence text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
        if (text == null || !homeCode(text.toString()).isEmpty()) {
            return null;
        }
        for (String word : text.toString().split("\\s+")) {
            if (!AddContact.is(word)) {
                continue;
            }
            Map<String, Object> read = Core.code(word);
            if (!read.containsKey("error") && !"mailbox".equals(Core.str(read, "kind"))) {
                return word;
            }
        }
        return null;
    }

    /**
     * A code shared to the app: into the form. True when it carries the
     * address, so joining needs nothing else.
     */
    private boolean shared(Intent intent) {
        if (intent == null || !Intent.ACTION_SEND.equals(intent.getAction())) {
            return false;
        }
        CharSequence text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
        String found = text == null ? "" : homeCode(text.toString());
        if (found.isEmpty()) {
            return false;
        }
        code.setText(found);
        return found.indexOf('@') > 0;
    }

    /** A code the scanner read: into the form. True when it carries the address. */
    private boolean scanned(Intent intent) {
        String text = intent == null ? null : intent.getStringExtra(CODE);
        String found = text == null ? "" : homeCode(text);
        if (found.isEmpty()) {
            return false;
        }
        code.setText(found);
        return found.indexOf('@') > 0;
    }

    /** The first word that starts like a home code, in either case; empty for none. */
    static String homeCode(String text) {
        for (String word : text.split("\\s+")) {
            if (word.regionMatches(true, 0, "lqxh1", 0, 5)) {
                return word;
            }
        }
        return "";
    }

    /** The address behind a combined code, into an empty address field. */
    private void fillServer(String text) {
        // The field follows the code's tail while it holds what the code
        // gave it (1464, ServerField): typed one character at a time, the
        // code used to leave the first character after its `@` there.
        String field = server.getText().toString();
        String next = ServerField.follow(text, field, serverGiven);
        if (!next.equals(field)) {
            server.setText(next);
            serverGiven = next.trim();
        }
        // The code's servers are the code's business (1471): shown, not
        // edited, and since 1448 not even shown: the field goes with
        // Advanced while the code carries any.
        server.setEnabled(!ServerField.carried(text));
        showAdvanced();
    }

    /** Advanced and the address field, as the code and the last start have it (1448). */
    private void showAdvanced() {
        boolean offered = JoinForm.advancedOffered(code.getText().toString());
        boolean open = offered
                && JoinForm.advancedOpen(advancedOpened, lastNeed, server.getText().toString());
        advanced.setVisibility(offered ? View.VISIBLE : View.GONE);
        advanced.setText(getString(R.string.advanced) + (open ? "  ▴" : "  ▾"));
        serverBox.setVisibility(open ? View.VISIBLE : View.GONE);
    }

    /** What the code gave the server field last (1464). */
    private String serverGiven = "";

    private String home() {
        return getFilesDir().getAbsolutePath();
    }

    /** Start the session, with what the form holds when asked to join. */
    private void start(final boolean joining) {
        final String typedCode = joining ? code.getText().toString().trim() : null;
        // A code with servers names none here: the library takes the
        // code's list whole, in its order (1471).
        final String typedServer = joining
                ? ServerField.request(typedCode, server.getText().toString()) : null;
        final boolean first = typedCode != null && !typedCode.isEmpty();
        asked = joining;
        join.setEnabled(false);
        retry.setVisibility(View.GONE);
        detailsToggle.setVisibility(View.GONE);
        details.setVisibility(View.GONE);
        error.setText(joining ? getString(R.string.joining) : getString(R.string.starting));
        final String home = home();
        new Thread(new Runnable() {
            @Override
            public void run() {
                Core.sense(getApplicationContext());
                final Map<String, Object> answer = Core.start(
                        home,
                        typedServer == null || typedServer.isEmpty() ? null : typedServer,
                        typedCode == null || typedCode.isEmpty() ? null : typedCode,
                        Build.MODEL);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        started(answer, first);
                    }
                });
            }
        }, "loquax-start").start();
    }

    /**
     * "Use this phone again" (1281): the mailbox runs on the new phone, so the warning names the
     * risk of two live copies before the old phone starts it again.
     */
    private void askTakeBack() {
        new android.app.AlertDialog.Builder(this)
                .setTitle(R.string.take_back)
                .setMessage(R.string.take_back_warning)
                .setNegativeButton(android.R.string.cancel, null)
                .setPositiveButton(R.string.take_back, new android.content.DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(android.content.DialogInterface shown, int which) {
                        takeBack();
                    }
                })
                .show();
    }

    private void takeBack() {
        moved = false;
        asked = true;
        retry.setVisibility(View.GONE);
        error.setText(getString(R.string.starting));
        final String home = home();
        new Thread(new Runnable() {
            @Override
            public void run() {
                Core.sense(getApplicationContext());
                final Map<String, Object> answer = Core.takeBack(home);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        started(answer, false);
                    }
                });
            }
        }, "loquax-take-back").start();
    }

    /** The mailbox's own words under Details, open or folded. */
    private void showDetails(boolean open) {
        detailsToggle.setText(getString(R.string.details) + (open ? "  ▴" : "  ▾"));
        details.setVisibility(open && !details.getText().toString().isEmpty() ? View.VISIBLE : View.GONE);
    }

    /** The start's answer; `joined` when a code was handed in, the phone's first join. */
    private void started(Map<String, Object> started, boolean joined) {
        join.setEnabled(true);
        if (Core.bool(started, "ok")) {
            Events.start();
            // The session outlives this screen from here on.
            LoquaxService.start(this);
            if (joined && imto == null) {
                // Joined just now: the address page once, so the user sees
                // their QR and their name (walk finding 13); its Done goes
                // on to the conversations.
                startActivity(new Intent(this, WelcomeActivity.class)
                        .putExtra(WelcomeActivity.SHOW, WelcomeActivity.ADDRESS_PAGE));
                finish();
                return;
            }
            Intent list = new Intent(this, ConversationsActivity.class);
            if (contact != null) {
                list.putExtra(ConversationsActivity.ADD_CODE, contact);
                contact = null;
            }
            startActivity(list);
            if (imto != null) {
                startActivity(new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, imto));
            }
            finish();
            return;
        }
        String need = Core.str(started, "need");
        moved = "moved".equals(need);
        if (moved) {
            // Walk finding 24 (1281): this phone sends nothing until its
            // owner takes the mailbox back.
            form.setVisibility(View.GONE);
            error.setText(getString(R.string.moved_title) + "\n\n" + Core.str(started, "error"));
            retry.setText(R.string.take_back);
            retry.setVisibility(View.VISIBLE);
            detailsToggle.setVisibility(View.GONE);
            return;
        }
        lastNeed = need;
        form.setVisibility(View.VISIBLE);
        showAdvanced();
        if (!JoinForm.saysFailure(asked, need)) {
            // The screen's own look without a code (1448, walk finding 4):
            // the bare form, nothing failed yet.
            error.setText("");
            retry.setVisibility(View.GONE);
            detailsToggle.setVisibility(View.GONE);
            details.setText("");
            showDetails(false);
            code.requestFocus();
            return;
        }
        retry.setText(R.string.try_again);
        boolean codesServers = JoinForm.codesServersFailed(need, code.getText().toString());
        error.setText(codesServers
                ? Core.str(started, "error") + "\n\n" + getString(R.string.join_code_servers_failed)
                : Core.str(started, "error"));
        retry.setVisibility(View.VISIBLE);
        String said = Core.str(started, "details");
        details.setText(said);
        detailsToggle.setVisibility(said.isEmpty() ? View.GONE : View.VISIBLE);
        showDetails(false);
        if (codesServers) {
            // The servers are the code's (1471): the code is what to edit.
            code.requestFocus();
        } else if ("server".equals(need)) {
            server.requestFocus();
        } else if ("code".equals(need)) {
            code.requestFocus();
        }
    }
}
