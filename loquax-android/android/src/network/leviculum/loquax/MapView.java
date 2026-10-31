package network.leviculum.loquax;

import android.content.Context;
import android.content.res.Configuration;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.os.PowerManager;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.ScaleGestureDetector;
import android.view.View;
import java.util.Map;

/**
 * The map, drawn by the library (loquax-android/src/map.rs) and hosted
 * here as a bitmap: Rust draws the regions, the names and the pins, this
 * view only shows the last frame and says what the finger did.
 *
 * One finger pans, a pinch or a double tap zooms. The finger's gestures
 * move and scale the last frame on screen at once, and a new frame is
 * asked for as soon as the frame thread is free, so frames come at the
 * rate the renderer goes while the finger moves (1131): each is drawn a
 * quarter screen past every edge, so a pan first reveals drawn map, at the
 * zoom at rest (1134), while a pinch moves at its tile step one to one,
 * which this view scales to the fingers' zoom (1155), composed from the
 * library's tile cache straight into the spare bitmap, with the names its
 * last frame at rest placed. A frame that lands is placed by
 * where the view it shows lies in the view the finger made, so the map
 * stays under the finger; the zoom the fingers leave is the zoom the map
 * keeps, nothing snaps. The request is built when the thread is free, from
 * the gestures up to then, so a newer view supersedes the ones it passed
 * and nothing is lost.
 *
 * When a frame at rest owes no tile and nothing else is owed, the frame
 * thread asks the library to prefetch the tiles around the view (1135), a
 * tile a worker a call, the ones ahead of the last pan first, until the
 * plan is done or a gesture or a new view comes: one call at most stands
 * between a gesture and its frame. Only while the window is shown and the
 * battery saver is off; the background service never draws.
 */
final class MapView extends View {
    /** What a frame said, on the UI thread. */
    interface Listener {
        void onShown(Map<String, Object> shown);
    }

    /**
     * The frame's margin past each edge, as a share of the screen: a
     * quarter drew a 1080 x 2400 frame moving at 64 ms on the host, half
     * at 111 ms (1131), and a smaller frame that lands sooner leaves less
     * of a pan to cover.
     */
    private static final float OVERSCAN = 0.25f;

    private final String dir;
    private final Listener listener;
    private final Paint paint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final GestureDetector gestures;
    private final ScaleGestureDetector pinch;

    // Everything below is guarded by `this`: the UI thread writes the
    // gestures, the frame thread reads them and writes the frame.
    private Bitmap bitmap;
    /** The bitmaps shown before it, reused once the screen let them go. */
    private Bitmap retired;
    private Bitmap spare;
    /** The frame's margins: the view's top left corner is this pixel of it. */
    private int marginX;
    private int marginY;
    /** The frame was drawn while the finger moved: its names were not placed anew. */
    private boolean bare;
    /** The last frame's view, its zoom in thousandths: absent before the first. */
    private Long x;
    private Long y;
    private Long zoomMilli;
    private String selected;
    /**
     * Where the frame is on screen: a pixel p of its view is at
     * scale * p + (offX, offY). The gestures change this at once; a frame
     * that lands is placed by it.
     */
    private float scale = 1f;
    private float offX;
    private float offY;
    /** A finger is on the map. */
    private boolean touching;
    private Integer tapX;
    private Integer tapY;
    private Long atLat;
    private Long atLon;
    /** A frame is owed: the view changed since the last request. */
    private boolean owed;
    /** The last frame was at rest and the library has tiles to prefetch. */
    private boolean idle;
    /** The window is shown: the app is in front. */
    private boolean visible;
    private Thread frames;

    MapView(Context context, String dir, Listener listener) {
        super(context);
        this.dir = dir;
        this.listener = listener;
        gestures = new GestureDetector(context, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                return true;
            }

            @Override
            public boolean onScroll(MotionEvent a, MotionEvent b, float dx, float dy) {
                synchronized (MapView.this) {
                    offX -= dx;
                    offY -= dy;
                }
                invalidate();
                ask();
                return true;
            }

            @Override
            public boolean onSingleTapConfirmed(MotionEvent e) {
                synchronized (MapView.this) {
                    tapX = Math.round((e.getX() - offX) / scale);
                    tapY = Math.round((e.getY() - offY) / scale);
                }
                ask();
                return true;
            }

            @Override
            public boolean onDoubleTap(MotionEvent e) {
                zoomAbout(2f, e.getX(), e.getY());
                return true;
            }
        });
        pinch = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScale(ScaleGestureDetector detector) {
                zoomAbout(detector.getScaleFactor(), detector.getFocusX(), detector.getFocusY());
                return true;
            }
        });
    }

    /** Scale what is on screen by `by` about the screen point (fx, fy). */
    private void zoomAbout(float by, float fx, float fy) {
        synchronized (this) {
            scale *= by;
            offX = by * (offX - fx) + fx;
            offY = by * (offY - fy) + fy;
        }
        invalidate();
        ask();
    }

    /**
     * Centre on a place, degrees times a million, as a message's fix opens
     * the map, with the pin of `peer` selected when it is not null.
     */
    synchronized void centreOn(long latE6, long lonE6, String peer) {
        atLat = latE6;
        atLon = lonE6;
        if (peer != null) {
            selected = peer;
        }
    }

    /** Draw the view anew: the pins changed, or a region is loading. */
    void ask() {
        synchronized (this) {
            owed = true;
            notifyAll();
        }
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        pinch.onTouchEvent(event);
        gestures.onTouchEvent(event);
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN) {
            synchronized (this) {
                touching = true;
                idle = false;
            }
        } else if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            boolean again;
            synchronized (this) {
                touching = false;
                // The names are placed anew over the view at rest.
                again = bare || scale != 1f || offX != 0 || offY != 0;
            }
            if (again) {
                ask();
            }
        }
        return true;
    }

    @Override
    protected void onSizeChanged(int w, int h, int oldw, int oldh) {
        super.onSizeChanged(w, h, oldw, oldh);
        ask();
    }

    @Override
    protected void onWindowVisibilityChanged(int visibility) {
        super.onWindowVisibilityChanged(visibility);
        synchronized (this) {
            visible = visibility == View.VISIBLE;
            notifyAll();
        }
    }

    /** The battery saver is on: no work ahead of the user. */
    private boolean saving() {
        PowerManager power = (PowerManager) getContext().getSystemService(Context.POWER_SERVICE);
        return power != null && power.isPowerSaveMode();
    }

    @Override
    protected void onAttachedToWindow() {
        super.onAttachedToWindow();
        frames = new Thread(new Runnable() {
            @Override
            public void run() {
                drawFrames();
            }
        }, "loquax-map");
        frames.start();
    }

    @Override
    protected void onDetachedFromWindow() {
        if (frames != null) {
            frames.interrupt();
            frames = null;
        }
        super.onDetachedFromWindow();
    }

    @Override
    protected synchronized void onDraw(Canvas canvas) {
        if (bitmap == null) {
            return;
        }
        canvas.save();
        canvas.translate(offX, offY);
        canvas.scale(scale, scale);
        canvas.drawBitmap(bitmap, -marginX, -marginY, paint);
        canvas.restore();
    }

    private boolean dark() {
        int night = getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    /** A bitmap of `w` x `h` for the next frame: the spare when it fits. */
    private Bitmap canvasFor(int w, int h) {
        synchronized (this) {
            Bitmap reuse = spare;
            spare = null;
            if (reuse != null && reuse.getWidth() == w && reuse.getHeight() == h) {
                return reuse;
            }
        }
        return Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888);
    }

    /**
     * The frame thread: wait until a frame is owed, ask the library, show
     * it; while none is owed and the view is idle, prefetch.
     */
    private void drawFrames() {
        while (!Thread.currentThread().isInterrupted()) {
            Map<String, Object> request;
            boolean moving;
            int w;
            int h;
            boolean ahead;
            synchronized (this) {
                while (!owed || getWidth() == 0 || getHeight() == 0) {
                    if (idle && visible && !touching && getWidth() > 0 && getHeight() > 0) {
                        break;
                    }
                    try {
                        wait();
                    } catch (InterruptedException e) {
                        return;
                    }
                }
                ahead = !owed;
                w = getWidth();
                h = getHeight();
            }
            if (ahead) {
                prefetch(w, h);
                continue;
            }
            int mx;
            int my;
            synchronized (this) {
                owed = false;
                idle = false;
                moving = touching;
                w = getWidth();
                h = getHeight();
                mx = Math.round(w * OVERSCAN);
                my = Math.round(h * OVERSCAN);
                // The frame's pixel under the screen's centre becomes the
                // new frame's centre, at the scale's exact zoom.
                float cx = w / 2f;
                float cy = h / 2f;
                int panX = Math.round((cx - offX) / scale - cx);
                int panY = Math.round((cy - offY) / scale - cy);
                long byMilli = Math.round(1000 * Math.log(scale) / Math.log(2));
                request = Core.map("dir", dir,
                        "w", (long) w, "h", (long) h,
                        "margin_x", (long) mx,
                        "margin_y", (long) my,
                        "moving", moving,
                        "density_milli", (long) Math.round(getResources().getDisplayMetrics().density * 1000),
                        "dark", dark(),
                        "pan_x", (long) panX, "pan_y", (long) panY,
                        "zoom_by_milli", byMilli,
                        "about_x", (long) (w / 2), "about_y", (long) (h / 2));
                if (x != null) {
                    request.put("x", x);
                    request.put("y", y);
                    request.put("zoom_milli", zoomMilli);
                }
                if (selected != null) request.put("selected", selected);
                if (tapX != null) {
                    request.put("tap_x", (long) tapX);
                    request.put("tap_y", (long) tapY);
                }
                if (atLat != null) {
                    request.put("at_lat_e6", atLat);
                    request.put("at_lon_e6", atLon);
                }
                tapX = null;
                tapY = null;
                atLat = null;
                atLon = null;
            }
            // The frame is the view and the margins on both sides (the
            // margins are at most a quarter screen, within the library's
            // clamp of a screen): the library draws it straight into the
            // bitmap, which nothing shows while it does (1155).
            Bitmap next = canvasFor(w + 2 * mx, h + 2 * my);
            final Map<String, Object> shown = Core.renderMap(request, next);
            if (Core.bool(shown, "ok")) {
                synchronized (this) {
                    place(shown, w, h, moving);
                    spare = retired;
                    retired = bitmap;
                    bitmap = next;
                    bare = moving;
                    marginX = (int) Core.num(shown, "margin_x");
                    marginY = (int) Core.num(shown, "margin_y");
                    x = Core.num(shown, "x");
                    y = Core.num(shown, "y");
                    zoomMilli = Core.num(shown, "zoom_milli");
                    Object chosen = shown.get("selected");
                    selected = chosen == null ? null : String.valueOf(chosen);
                }
            } else {
                synchronized (this) {
                    // Not shown: the next frame draws into it.
                    spare = next;
                }
            }
            post(new Runnable() {
                @Override
                public void run() {
                    invalidate();
                    listener.onShown(shown);
                }
            });
            synchronized (this) {
                idle = !moving && Core.bool(shown, "prefetching")
                        && !Core.bool(shown, "loading");
            }
            if (Core.bool(shown, "pending")) {
                // Tiles the frame showed from another step: draw them now.
                ask();
            }
            if (Core.bool(shown, "loading")) {
                postDelayed(new Runnable() {
                    @Override
                    public void run() {
                        ask();
                    }
                }, 1500);
            }
        }
    }

    /**
     * One batch of the prefetch around the last frame at rest: idle ends
     * when the plan is done or the battery saver is on.
     */
    private void prefetch(int w, int h) {
        if (saving()) {
            synchronized (this) {
                idle = false;
            }
            return;
        }
        Map<String, Object> request = Core.map("dir", dir, "prefetch", true,
                "w", (long) w, "h", (long) h,
                "density_milli", (long) Math.round(getResources().getDisplayMetrics().density * 1000));
        Map<String, Object> answer = Core.renderMap(request, null);
        synchronized (this) {
            idle = idle && Core.bool(answer, "ok") && Core.bool(answer, "prefetching");
        }
    }

    /**
     * Place the frame that landed, `shown`, asked for a `w` x `h` screen:
     * where its view lies in the last frame's, carried through what the
     * fingers did since, so the map stays under them. A frame asked at
     * rest, with no finger down and nothing asked since, is shown as
     * drawn: its zoom is the one the fingers left, to a thousandth of a
     * step, its centre to half a pixel. Called holding `this`, before the
     * last frame's view is replaced.
     */
    private void place(Map<String, Object> shown, int w, int h, boolean moving) {
        long z1 = Core.num(shown, "zoom_milli");
        boolean first = x == null || zoomMilli == null || y == null;
        if (first || (!moving && !touching && !owed)) {
            scale = 1f;
            offX = 0;
            offY = 0;
            return;
        }
        // A pixel q of the new view is the pixel p = a q + b of the last
        // one: a the ratio of their scales, b from their centres' distance
        // in the last view's pixels, x across the antimeridian the short way.
        double a = Math.pow(2, (zoomMilli - z1) / 1000.0);
        double unit = Math.pow(2, 24 - zoomMilli / 1000.0);
        long dx = Core.num(shown, "x") - x;
        dx = ((dx + (1L << 31)) & 0xffffffffL) - (1L << 31);
        long dy = Core.num(shown, "y") - y;
        double bx = (1 - a) * (w / 2.0) + dx / unit;
        double by = (1 - a) * (h / 2.0) + dy / unit;
        offX = (float) (scale * bx + offX);
        offY = (float) (scale * by + offY);
        scale = (float) (scale * a);
    }
}
