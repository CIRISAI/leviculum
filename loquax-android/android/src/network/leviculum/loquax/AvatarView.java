package network.leviculum.loquax;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.BitmapShader;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.Shader;
import android.graphics.Typeface;
import android.util.AttributeSet;
import android.util.LruCache;
import android.view.View;
import java.io.File;
import java.util.Map;

/**
 * A peer's avatar: the linked contact's photo as loquax copied it (1139),
 * else a circle with its initials (1129). The colours are the peer's icon
 * appearance when it sent one, else one of eight picked by the address,
 * each with white letters at 5:1 or more.
 */
public class AvatarView extends View {
    /** White on each is 5.1:1 to 6.0:1 (the contrast table in 1129's report). */
    private static final int[] PALETTE = {
        0xff8e4b8a, 0xff3d6fb6, 0xffb0503a, 0xff2f7a6e,
        0xff7a5fb8, 0xffa2526a, 0xff5b6b2f, 0xff8a6420,
    };

    /** Decoded photos by path and time, so a list's rows do not decode on every bind. */
    private static final LruCache<String, Bitmap> PHOTOS = new LruCache<>(32);

    private final Paint circle = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint letters = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint picture = new Paint(Paint.ANTI_ALIAS_FLAG | Paint.FILTER_BITMAP_FLAG);
    private final Paint dot = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint ring = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint live = new Paint(Paint.ANTI_ALIAS_FLAG);
    private String initials = "";
    /** The liveness ring's colour (1209), 0 for none. */
    private int liveRing;
    private Bitmap photo;
    /** The status dot on the lower right edge (1199): null draws none. */
    private Boolean up;

    public AvatarView(Context context, AttributeSet attrs) {
        super(context, attrs);
        letters.setTextAlign(Paint.Align.CENTER);
        letters.setTypeface(Typeface.create(Typeface.DEFAULT, Typeface.BOLD));
    }

    /** Show the peer `peer` (hex) as `row` of the facade describes it. */
    void show(String peer, Map<String, Object> row) {
        boolean nearby = Core.bool(row, "nearby");
        initials = nearby ? "" : Core.str(row, "initials");
        int[] colours = colours(peer, row);
        circle.setColor(colours[0]);
        letters.setColor(colours[1]);
        photo = nearby ? null : photo(Core.str(row, "photo"));
        liveRing = ring(getContext(), row);
        setContentDescription(Core.str(row, "name"));
        invalidate();
    }

    /**
     * The liveness ring of the peer `row` describes (1209), the colour the
     * library names in its `ring` (1217, loquax's `scene::Ring`): 0 (none)
     * for a row without one.
     */
    static int ring(Context context, Map<String, Object> row) {
        switch (Core.str(row, "ring")) {
            case "accent": return context.getColor(R.color.accent);
            case "secondary": return context.getColor(R.color.text_secondary);
            default: return 0;
        }
    }

    /**
     * A node nearby that is no contact (1302): a neutral grey circle, no
     * letters and no photo, so it never looks like a friend.
     */
    static final int NEARBY = 0xFF9E9E9E;

    /** The ring's width as a share of the avatar's side: thin. */
    static final float RING = 0.06f;

    /**
     * The circle's and the letters' colours of `peer` as `row` describes
     * it: its own icon appearance, else one of the palette by the address
     * with white letters.
     */
    static int[] colours(String peer, Map<String, Object> row) {
        if (Core.bool(row, "nearby")) {
            return new int[] {NEARBY, Color.WHITE};
        }
        int bg = colour(Core.str(row, "icon_bg"));
        int fg = colour(Core.str(row, "icon_fg"));
        if (bg == 0 || fg == 0) {
            int at = 0;
            try {
                at = peer.length() >= 2 ? Integer.parseInt(peer.substring(0, 2), 16) % PALETTE.length : 0;
            } catch (NumberFormatException e) {
                at = 0;
            }
            bg = PALETTE[at];
            fg = Color.WHITE;
        }
        return new int[] {bg, fg};
    }

    /**
     * `peer` as `row` describes it, a circle `side` wide about (cx, cy)
     * on `canvas`, with the caller's paints: the map's avatar pins draw
     * the list's circle (1200).
     */
    static void drawAt(Canvas canvas, float cx, float cy, float side, String peer,
            Map<String, Object> row, Paint circle, Paint letters, Paint picture) {
        int[] colours = colours(peer, row);
        circle.setColor(colours[0]);
        letters.setColor(colours[1]);
        letters.setTextAlign(Paint.Align.CENTER);
        letters.setTypeface(Typeface.create(Typeface.DEFAULT, Typeface.BOLD));
        boolean nearby = Core.bool(row, "nearby");
        paint(canvas, cx, cy, side, nearby ? null : photo(Core.str(row, "photo")),
                nearby ? "" : Core.str(row, "initials"), circle, letters, picture);
    }

    /** The photo's middle square in the circle, or the circle with the initials. */
    private static void paint(Canvas canvas, float cx, float cy, float side, Bitmap photo,
            String initials, Paint circle, Paint letters, Paint picture) {
        if (photo != null && side > 0) {
            // The photo's middle square, scaled to the circle.
            BitmapShader shader = new BitmapShader(photo, Shader.TileMode.CLAMP, Shader.TileMode.CLAMP);
            float scale = side / Math.min(photo.getWidth(), photo.getHeight());
            Matrix place = new Matrix();
            place.setScale(scale, scale);
            place.postTranslate(cx - photo.getWidth() * scale / 2f, cy - photo.getHeight() * scale / 2f);
            shader.setLocalMatrix(place);
            picture.setShader(shader);
            canvas.drawCircle(cx, cy, side / 2f, picture);
            return;
        }
        canvas.drawCircle(cx, cy, side / 2f, circle);
        letters.setTextSize(side * (initials.codePointCount(0, initials.length()) > 1 ? 0.38f : 0.45f));
        Paint.FontMetrics m = letters.getFontMetrics();
        canvas.drawText(initials, cx, cy - (m.ascent + m.descent) / 2f, letters);
    }

    /**
     * The bar's status dot (1199): green while the mailbox link is up,
     * grey otherwise, in a ring of the background so it reads on any
     * avatar; null for none.
     */
    void dot(Boolean linkUp) {
        up = linkUp;
        if (linkUp != null) {
            dot.setColor(getContext().getColor(linkUp ? R.color.status_up : R.color.status_down));
            ring.setColor(getContext().getColor(R.color.background));
        }
        invalidate();
    }

    /** The photo at `path`, decoded once per change of the file; null for none. */
    static Bitmap photo(String path) {
        if (path == null || path.isEmpty()) {
            return null;
        }
        File file = new File(path);
        String key = path + "@" + file.lastModified();
        Bitmap cached = PHOTOS.get(key);
        if (cached == null && file.isFile()) {
            cached = BitmapFactory.decodeFile(path);
            if (cached != null) {
                PHOTOS.put(key, cached);
            }
        }
        return cached;
    }

    /** An `rrggbb` as an opaque colour, 0 for anything else. */
    private static int colour(String hex) {
        if (hex.length() != 6) {
            return 0;
        }
        try {
            return 0xff000000 | Integer.parseInt(hex, 16);
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    @Override
    protected void onDraw(Canvas canvas) {
        float side = Math.min(getWidth(), getHeight());
        float cx = getWidth() / 2f;
        float cy = getHeight() / 2f;
        if (liveRing != 0 && side > 0) {
            // The ring on the edge, the avatar inside it with a gap of the
            // background, so a photo's edge never reads as the ring.
            live.setStyle(Paint.Style.STROKE);
            live.setStrokeWidth(side * RING);
            live.setColor(liveRing);
            canvas.drawCircle(cx, cy, side / 2f - side * RING / 2f, live);
            side = side * (1f - 3f * RING);
        }
        paint(canvas, cx, cy, side, photo, initials, circle, letters, picture);
        if (up != null && side > 0) {
            // On the circle's edge at the lower right, pulled in enough
            // that the ring stays inside the view.
            float at = side / 2f * 0.6f;
            canvas.drawCircle(cx + at, cy + at, side * 0.17f, ring);
            canvas.drawCircle(cx + at, cy + at, side * 0.12f, dot);
        }
    }
}
