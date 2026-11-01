package network.leviculum.loquax;

import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.os.Bundle;
import android.util.DisplayMetrics;
import android.view.View;
import android.widget.ImageView;
import android.widget.Toast;

/**
 * A photo whole (1220): the tap on a bubble's photo opens it, a tap or
 * Back closes it. The library decodes and scales it to the screen's
 * longer side (`Query::Photo`); this screen only shows the Bitmap.
 */
public class PhotoActivity extends Activity {
    static final String PEER = "peer";
    static final String KEY = "key";

    private ImageView image;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        image = new ImageView(this);
        image.setId(R.id.photo_full);
        image.setBackgroundColor(Color.BLACK);
        image.setScaleType(ImageView.ScaleType.FIT_CENTER);
        image.setContentDescription(getString(R.string.scene_view_photo));
        image.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        setContentView(image);
        final String peer = getIntent().getStringExtra(PEER);
        final long key = getIntent().getLongExtra(KEY, 0);
        DisplayMetrics screen = getResources().getDisplayMetrics();
        final int edge = Math.min(4096, Math.max(screen.widthPixels, screen.heightPixels));
        new Thread(new Runnable() {
            @Override
            public void run() {
                final Bitmap bitmap = Core.photoBitmap(peer, key, edge);
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
                        image.setImageBitmap(bitmap);
                    }
                });
            }
        }, "photo").start();
    }
}
