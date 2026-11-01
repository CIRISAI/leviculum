package network.leviculum.loquax;

import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.os.Bundle;
import android.util.DisplayMetrics;
import android.widget.Toast;

/**
 * A photo whole (1220): the tap on a bubble's photo opens it. It zooms
 * under two fingers (1269, {@link PhotoView}); at 1x a tap, a swipe down
 * or Back closes it, zoomed Back goes to 1x first. The library decodes
 * and scales it to twice the screen's longer side (`Query::Photo`), so a
 * zoom has pixels to show; this screen only shows the Bitmap.
 */
public class PhotoActivity extends Activity {
    static final String PEER = "peer";
    static final String KEY = "key";

    private PhotoView image;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        image = new PhotoView(this, new PhotoView.Listener() {
            @Override
            public void close() {
                finish();
            }
        });
        image.setId(R.id.photo_full);
        image.setBackgroundColor(Color.BLACK);
        image.setContentDescription(getString(R.string.scene_view_photo));
        setContentView(image);
        final String peer = getIntent().getStringExtra(PEER);
        final long key = getIntent().getLongExtra(KEY, 0);
        DisplayMetrics screen = getResources().getDisplayMetrics();
        final int edge = Math.min(4096, 2 * Math.max(screen.widthPixels, screen.heightPixels));
        new Thread(new Runnable() {
            @Override
            public void run() {
                final int[] full = new int[2];
                final Bitmap bitmap = Core.photoBitmap(peer, key, edge, full);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (isFinishing()) {
                            return;
                        }
                        if (bitmap == null) {
                            Toast.makeText(PhotoActivity.this, R.string.photo_unreadable,
                                    Toast.LENGTH_LONG).show();
                            finish();
                            return;
                        }
                        image.show(bitmap, full[0], full[1]);
                    }
                });
            }
        }, "photo").start();
    }

    @Override
    public void onBackPressed() {
        if (image.back()) {
            super.onBackPressed();
        }
    }
}
