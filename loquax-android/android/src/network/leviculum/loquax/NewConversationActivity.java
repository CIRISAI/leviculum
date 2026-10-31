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
 * A new conversation, from the list's pencil (1130): the address pasted or
 * typed, opened as the list's field did before. A contact code
 * (`lxma://`, 1187) pasted here adds the contact under the name asked for
 * and opens its chat; the scanner is on the profile.
 */
public class NewConversationActivity extends Activity {
    private EditText address;
    private TextView error;
    private final ServiceLink link = new ServiceLink(this);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_new);
        address = (EditText) findViewById(R.id.address);
        error = (TextView) findViewById(R.id.error);
        ImageButton back = (ImageButton) findViewById(R.id.back);
        back.setImageDrawable(new Glyph(Glyph.Kind.BACK, getColor(R.color.text)));
        back.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        ((Button) findViewById(R.id.paste)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                paste();
            }
        });
        ((Button) findViewById(R.id.open)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                open();
            }
        });
        address.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int action, KeyEvent event) {
                if (action == EditorInfo.IME_ACTION_GO) {
                    open();
                    return true;
                }
                return false;
            }
        });
        address.requestFocus();
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

    /** The clipboard's text into the field, in place of what it held. */
    private void paste() {
        ClipboardManager clipboard = getSystemService(ClipboardManager.class);
        ClipData clip = clipboard == null ? null : clipboard.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) {
            return;
        }
        CharSequence text = clip.getItemAt(0).coerceToText(this);
        if (text != null) {
            address.setText(text.toString().trim());
            address.setSelection(address.getText().length());
        }
    }

    /** The conversation with the address, or why not under the field. */
    private void open() {
        String peer = address.getText().toString().trim();
        if (peer.isEmpty()) {
            return;
        }
        if (AddContact.is(peer)) {
            AddContact.ask(this, peer, new Runnable() {
                @Override
                public void run() {
                    finish();
                }
            });
            return;
        }
        String why = Core.open(peer);
        if (why != null) {
            error.setText(why);
            return;
        }
        startActivity(new Intent(this, ChatActivity.class).putExtra(ChatActivity.PEER, peer));
        finish();
    }
}
