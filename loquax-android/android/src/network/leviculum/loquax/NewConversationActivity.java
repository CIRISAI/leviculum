package network.leviculum.loquax;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.os.Bundle;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageButton;
import android.widget.TextView;

/**
 * A new conversation, from the list's pencil (1130, 1263 after the
 * ninety percent walk): "Scan a code" first, "Paste a code" second, both
 * read by the library (loquax::code) as every scanner is. A friend's code,
 * a contact code (`lxma://`, 1187) or an address alone, adds the friend
 * under the name asked for and opens the chat; a mailbox's code goes to
 * the join; anything else is said in one sentence under the rows. Typing
 * an address waits under Advanced, collapsed.
 */
public class NewConversationActivity extends Activity {
    private static final int SCAN = 1;
    private static final String ADVANCED_OPEN = "advanced";

    private EditText address;
    private TextView error;
    private TextView advanced;
    private View advancedBody;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_new);
        address = (EditText) findViewById(R.id.address);
        error = (TextView) findViewById(R.id.error);
        advanced = (TextView) findViewById(R.id.new_advanced);
        advancedBody = findViewById(R.id.new_advanced_body);
        ImageButton back = (ImageButton) findViewById(R.id.back);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, getColor(R.color.text)));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        View scan = findViewById(R.id.new_scan);
        if (ScanActivity.available(this)) {
            scan.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    startActivityForResult(new Intent(NewConversationActivity.this, ScanActivity.class)
                            .putExtra(ScanActivity.PREFIX, ScanActivity.CODES), SCAN);
                }
            });
        } else {
            scan.setVisibility(View.GONE);
        }
        findViewById(R.id.new_paste).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                paste();
            }
        });
        advanced.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showAdvanced(advancedBody.getVisibility() != View.VISIBLE);
            }
        });
        showAdvanced(savedInstanceState != null && savedInstanceState.getBoolean(ADVANCED_OPEN));
        ((Button) findViewById(R.id.open)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                take(address.getText().toString());
            }
        });
        address.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                if (action == EditorInfo.IME_ACTION_GO) {
                    take(address.getText().toString());
                    return true;
                }
                return false;
            }
        });
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        state.putBoolean(ADVANCED_OPEN, advancedBody.getVisibility() == View.VISIBLE);
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
    protected void onActivityResult(int request, int result, Intent data) {
        if (request == SCAN && result == RESULT_OK && data != null) {
            take(String.valueOf(data.getStringExtra(ScanActivity.TEXT)));
        }
    }

    /** Advanced open with the address field, or collapsed. */
    private void showAdvanced(boolean open) {
        advanced.setText(getString(R.string.advanced) + (open ? "  ▴" : "  ▾"));
        advancedBody.setVisibility(open ? View.VISIBLE : View.GONE);
        if (open) {
            address.requestFocus();
        }
    }

    /** The clipboard's text, taken as a code. */
    private void paste() {
        ClipboardManager clipboard = getSystemService(ClipboardManager.class);
        ClipData clip = clipboard == null ? null : clipboard.getPrimaryClip();
        CharSequence text = clip == null || clip.getItemCount() == 0
                ? null : clip.getItemAt(0).coerceToText(this);
        if (text == null || text.toString().trim().isEmpty()) {
            error.setText(R.string.new_nothing);
            return;
        }
        take(text.toString());
    }

    /** A code scanned, pasted or typed: the friend added, or why not under the rows. */
    private void take(String text) {
        if (text.trim().isEmpty()) {
            return;
        }
        String why = AddContact.take(this, text, new Runnable() {
            @Override
            public void run() {
                finish();
            }
        });
        error.setText(why == null ? "" : why);
    }
}
