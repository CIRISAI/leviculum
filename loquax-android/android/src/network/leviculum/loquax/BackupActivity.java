package network.leviculum.loquax;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Rect;
import android.graphics.Typeface;
import android.graphics.pdf.PdfDocument;
import android.net.Uri;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.print.PageRange;
import android.print.PrintAttributes;
import android.print.PrintDocumentAdapter;
import android.print.PrintDocumentInfo;
import android.print.PrintManager;
import android.text.InputType;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Map;

/**
 * "Back up your mailbox" (1152): the address of the mailbox on this phone,
 * sealed with a passphrase the user chooses (loquax/src/home/backup.rs),
 * as a file saved through the system's file picker (no storage
 * permission) and as a paper code to print. Both hold the same line, and
 * the first start restores from either ("I already have a Loquax
 * address"). The passphrase is asked twice and never kept.
 */
public class BackupActivity extends Activity {
    /** The shortest passphrase taken: the paper may lie where others see it. */
    static final int MIN_PASSPHRASE = 8;
    private static final int SAVE = 1;

    private EditText passphrase;
    private EditText again;
    private TextView said;
    private LinearLayout paper;
    private Map<String, Object> sealed;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        ScrollView frame = new ScrollView(this);
        frame.setFitsSystemWindows(true);
        frame.setBackgroundColor(getColor(R.color.background));
        LinearLayout body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(dp(24), dp(24), dp(24), dp(24));
        body.addView(text(getString(R.string.backup_title), 22, true), matchWrap());
        body.addView(gap(text(getString(R.string.backup_text), 15, false), 8, 12), matchWrap());
        passphrase = secret(getString(R.string.passphrase));
        body.addView(passphrase, matchWrap());
        again = secret(getString(R.string.passphrase_again));
        body.addView(again, matchWrap());
        Button save = button(getString(R.string.backup_file));
        save.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                seal(new Runnable() {
                    @Override
                    public void run() {
                        startActivityForResult(new Intent(Intent.ACTION_CREATE_DOCUMENT)
                                .addCategory(Intent.CATEGORY_OPENABLE)
                                .setType("text/plain")
                                .putExtra(Intent.EXTRA_TITLE, "loquax-backup.txt"), SAVE);
                    }
                });
            }
        });
        body.addView(save, wrap());
        Button print = button(getString(R.string.backup_paper));
        print.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                seal(new Runnable() {
                    @Override
                    public void run() {
                        showPaper();
                    }
                });
            }
        });
        body.addView(print, wrap());
        said = text("", 15, false);
        body.addView(gap(said, 12, 0), matchWrap());
        paper = new LinearLayout(this);
        paper.setOrientation(LinearLayout.VERTICAL);
        body.addView(paper, matchWrap());
        frame.addView(body);
        setContentView(frame);
    }

    /**
     * The line for the passphrase typed, twice the same, then `then`; or
     * the reason said. The library seals it on the engine's thread (1464).
     */
    private void seal(final Runnable then) {
        final String typed = passphrase.getText().toString();
        if (typed.length() < MIN_PASSPHRASE) {
            said.setText(getString(R.string.passphrase_short, MIN_PASSPHRASE));
            return;
        }
        if (!typed.equals(again.getText().toString())) {
            said.setText(R.string.passphrase_differs);
            return;
        }
        if (sealed != null && typed.equals(sealed.get("passphrase"))) {
            then.run();
            return;
        }
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.backup(typed);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (isFinishing() || isDestroyed()) {
                    return;
                }
                if (Core.str(answer, "line").isEmpty()) {
                    said.setText(Core.str(answer, "error"));
                    return;
                }
                answer.put("passphrase", typed);
                sealed = answer;
                said.setText("");
                then.run();
            }
        });
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        if (request != SAVE || result != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        Uri target = data.getData();
        try (OutputStream out = getContentResolver().openOutputStream(target)) {
            if (out == null) {
                throw new IOException(target.toString());
            }
            out.write(fileText().getBytes(StandardCharsets.UTF_8));
            said.setText(R.string.backup_saved);
        } catch (IOException | RuntimeException failed) {
            said.setText(getString(R.string.backup_not_saved, failed.getMessage()));
        }
    }

    /** What the file holds: a line saying what it is, then the backup line. */
    private String fileText() {
        return getString(R.string.backup_file_head) + "\n\n" + Core.str(sealed, "line") + "\n";
    }

    /** The paper code on the screen, with the button that prints it. */
    private void showPaper() {
        paper.removeAllViews();
        ImageView code = QrCode.view(this, sealed, dp(280), getString(R.string.backup_paper));
        if (code != null) {
            LinearLayout.LayoutParams at = new LinearLayout.LayoutParams(dp(280), dp(280));
            at.gravity = Gravity.CENTER_HORIZONTAL;
            at.topMargin = dp(16);
            paper.addView(code, at);
        }
        TextView line = text(Core.str(sealed, "line"), 12, false);
        line.setTypeface(Typeface.MONOSPACE);
        line.setTextIsSelectable(true);
        paper.addView(gap(line, 8, 8), matchWrap());
        Button print = button(getString(R.string.print));
        print.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                PrintManager printing = getSystemService(PrintManager.class);
                printing.print("Loquax", new Paper(BackupActivity.this,
                        QrCode.bitmap(sealed.get("qr"), (int) Core.num(sealed, "qr_size"), 600),
                        Core.str(sealed, "line"), getString(R.string.backup_paper_head)),
                        null);
            }
        });
        paper.addView(print, wrap());
    }

    /** One page: a heading, the code, the line below it in small type. */
    private static final class Paper extends PrintDocumentAdapter {
        private final Context context;
        private final Bitmap code;
        private final String line;
        private final String head;
        private PrintAttributes attributes;

        Paper(Context context, Bitmap code, String line, String head) {
            this.context = context;
            this.code = code;
            this.line = line;
            this.head = head;
        }

        @Override
        public void onLayout(PrintAttributes old, PrintAttributes now, CancellationSignal cancel,
                LayoutResultCallback callback, Bundle extras) {
            attributes = now;
            callback.onLayoutFinished(new PrintDocumentInfo.Builder("loquax-backup.pdf")
                    .setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT)
                    .setPageCount(1)
                    .build(), true);
        }

        @Override
        public void onWrite(PageRange[] pages, ParcelFileDescriptor destination,
                CancellationSignal cancel, WriteResultCallback callback) {
            PrintAttributes.MediaSize size = attributes == null || attributes.getMediaSize() == null
                    ? PrintAttributes.MediaSize.ISO_A4 : attributes.getMediaSize();
            // Mils to points: a point is a 72nd of an inch.
            int width = size.getWidthMils() * 72 / 1000;
            int height = size.getHeightMils() * 72 / 1000;
            PdfDocument document = new PdfDocument();
            PdfDocument.Page page = document.startPage(
                    new PdfDocument.PageInfo.Builder(width, height, 1).create());
            Canvas canvas = page.getCanvas();
            Paint ink = new Paint(Paint.ANTI_ALIAS_FLAG);
            ink.setTextSize(14);
            int margin = 48;
            float y = margin + 14;
            for (String row : head.split("\n")) {
                canvas.drawText(row, margin, y, ink);
                y += 20;
            }
            int side = Math.min(width - 2 * margin, 320);
            if (code != null) {
                int left = (width - side) / 2;
                canvas.drawBitmap(code, null,
                        new Rect(left, (int) y + 12, left + side, (int) y + 12 + side), null);
            }
            y += side + 40;
            ink.setTypeface(Typeface.MONOSPACE);
            ink.setTextSize(9);
            for (int at = 0; at < line.length(); at += 64) {
                canvas.drawText(line.substring(at, Math.min(line.length(), at + 64)),
                        margin, y, ink);
                y += 13;
            }
            document.finishPage(page);
            try (FileOutputStream out = new FileOutputStream(destination.getFileDescriptor())) {
                document.writeTo(out);
                callback.onWriteFinished(new PageRange[] {PageRange.ALL_PAGES});
            } catch (IOException failed) {
                callback.onWriteFailed(failed.getMessage());
            } finally {
                document.close();
            }
        }
    }

    private EditText secret(String hint) {
        EditText field = new EditText(this);
        field.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
        field.setHint(hint);
        field.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);
        return field;
    }

    private Button button(String words) {
        Button button = new Button(this);
        button.setText(words);
        button.setAllCaps(false);
        return button;
    }

    private TextView text(String words, float sp, boolean bold) {
        TextView view = new TextView(this);
        view.setText(words);
        view.setTextSize(sp);
        view.setTextColor(getColor(R.color.text));
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
        return params;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}
