package network.leviculum.loquax;

import android.content.Context;
import android.content.res.Configuration;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.Typeface;
import android.os.PowerManager;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.ScaleGestureDetector;
import android.view.View;
import android.view.animation.DecelerateInterpolator;
import android.animation.ValueAnimator;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
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
 *
 * The people are this view's own (1200): the library draws no pin and
 * says in each frame's marks where the friends and their clusters lie,
 * and this view draws them over the frame as the chat list's avatar
 * circles with the fix's age under them, a cluster as a circle with its
 * count, moved and scaled with the frame by the finger. A tap on one is
 * the listener's; a cluster's tap or a friend picked in the sheet flies
 * the map there, an animated pan and zoom that asks frames as a finger
 * would.
 *
 * The phone's own position (1201) is a dot in the accent with a ring of
 * the background, where the frame says the fix lies; the crosshair centres
 * on it at a zoom of its choosing (centreAt), and a finger that moves the
 * map tells the listener, which ends follow mode.
 */
final class MapView extends View {
    /** What a frame said, on the UI thread. */
    interface Listener {
        void onShown(Map<String, Object> shown);

        /** A pin or a cluster was tapped: a mark of the frame's. */
        void onMark(Map<String, Object> mark);

        /** A tap on the map away from every pin. */
        void onTapAway();

        /** A finger moved or zoomed the map (1201): follow mode ends. */
        void onFinger();
    }

    /** An avatar pin's diameter, as the library clusters them (friends.rs PIN_DP). */
    static final int PIN_DP = 40;
    /** How long a fly takes. */
    private static final long FLY_MS = 450;

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
    private final float density;
    private final Paint circle = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint letters = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint picture = new Paint(Paint.ANTI_ALIAS_FLAG | Paint.FILTER_BITMAP_FLAG);
    private final Paint ring = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint label = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint labelGround = new Paint(Paint.ANTI_ALIAS_FLAG);
    /** The people the marks name, by peer: the Friends query's rows and ours (UI thread). */
    private Map<String, Map<String, Object>> people = new HashMap<>();
    /** The peer whose card is open: its pin gets the accent ring (UI thread). */
    private String carded;
    /** A fly is on: frames are asked as while a finger moves. */
    private boolean flying;
    private ValueAnimator fly;

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
    /** The last frame's marks (1200), in its view's pixels. */
    private List<Map<String, Object>> marks = Collections.emptyList();
    /** The last frame's view size and deepest zoom. */
    private int viewW;
    private int viewH;
    private long maxZoom = 20;
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
    private Long atLat;
    private Long atLon;
    /** The zoom to centre on atLat, atLon at, in thousandths; null lets the library fit. */
    private Long atZoom;
    /** The phone's own fix (1201), and where the last frame put it in its view. */
    private Long ownLat;
    private Long ownLon;
    private Long ownX;
    private Long ownY;
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
        density = context.getResources().getDisplayMetrics().density;
        ring.setColor(context.getColor(R.color.background));
        label.setColor(context.getColor(R.color.text));
        label.setTextSize(11 * context.getResources().getDisplayMetrics().scaledDensity);
        label.setTextAlign(Paint.Align.CENTER);
        labelGround.setColor(context.getColor(R.color.bar));
        gestures = new GestureDetector(context, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                return true;
            }

            @Override
            public boolean onScroll(MotionEvent a, MotionEvent b, float dx, float dy) {
                listener.onFinger();
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
                Map<String, Object> hit = markAt(e.getX(), e.getY());
                if (hit != null) {
                    listener.onMark(hit);
                } else {
                    listener.onTapAway();
                }
                return true;
            }

            @Override
            public boolean onDoubleTap(MotionEvent e) {
                listener.onFinger();
                zoomAbout(2f, e.getX(), e.getY());
                return true;
            }
        });
        pinch = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScale(ScaleGestureDetector detector) {
                listener.onFinger();
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

    /**
     * Centre on a place, degrees times a million, at `zoomMilli` (held to
     * the regions' zooms by the library): the crosshair (1201). Ends a
     * fly and drops what the fingers did to the frame on screen.
     */
    void centreAt(long latE6, long lonE6, long zoomMilli) {
        if (fly != null && fly.isRunning()) {
            fly.cancel();
        }
        synchronized (this) {
            flying = false;
            atLat = latE6;
            atLon = lonE6;
            atZoom = zoomMilli;
            owed = true;
            notifyAll();
        }
    }

    /** The phone's own fix, degrees times a million; null for none. */
    void own(Long latE6, Long lonE6) {
        synchronized (this) {
            ownLat = latE6;
            ownLon = lonE6;
            if (latE6 == null) {
                ownX = null;
                ownY = null;
            }
        }
        invalidate();
        ask();
    }

    /** The last frame's exact zoom in thousandths; 0 before the first. */
    synchronized long zoom() {
        return zoomMilli == null ? 0 : zoomMilli;
    }

    /** The people the marks name, by peer, and the peer whose card is open. */
    void people(Map<String, Map<String, Object>> rows, String card) {
        people = rows;
        carded = card;
        invalidate();
    }

    /** The mark under the screen point (x, y), the one drawn last first; null for none. */
    private synchronized Map<String, Object> markAt(float x, float y) {
        float reach = PIN_DP * density / 2f + 6 * density;
        for (int i = marks.size() - 1; i >= 0; i--) {
            Map<String, Object> mark = marks.get(i);
            float mx = offX + scale * Core.num(mark, "x");
            float my = offY + scale * Core.num(mark, "y");
            if (Math.hypot(mx - x, my - y) <= reach) {
                return mark;
            }
        }
        return null;
    }

    /** Where `mark` is on the screen now. */
    synchronized float[] onScreen(Map<String, Object> mark) {
        return new float[] {offX + scale * Core.num(mark, "x"), offY + scale * Core.num(mark, "y")};
    }

    /**
     * Fly to the cluster `mark` names: its place to the screen's centre,
     * zoomed to where its pins part.
     */
    void flyInto(Map<String, Object> mark) {
        float[] at = onScreen(mark);
        double steps;
        synchronized (this) {
            steps = (Core.num(mark, "split_milli") - (zoomMilli == null ? 0 : zoomMilli)) / 1000.0;
        }
        fly(at[0], at[1], Math.max(0, steps));
    }

    /**
     * Fly to the place (degrees times a million): an animated pan to it and
     * a zoom to z15 at least, the deepest the regions allow at most. A place
     * more than three screens away is jumped to, the library fitting the
     * view there: a pan that long would only blur.
     */
    void flyTo(long latE6, long lonE6) {
        float sx;
        float sy;
        double steps;
        synchronized (this) {
            if (x == null || zoomMilli == null || viewW == 0) {
                atLat = latE6;
                atLon = lonE6;
                owed = true;
                notifyAll();
                return;
            }
            double unit = Math.pow(2, 24 - zoomMilli / 1000.0);
            long[] world = world(latE6, lonE6);
            long dx = world[0] - x;
            dx = ((dx + (1L << 31)) & 0xffffffffL) - (1L << 31);
            long dy = world[1] - y;
            double px = viewW / 2.0 + dx / unit;
            double py = viewH / 2.0 + dy / unit;
            sx = (float) (offX + scale * px);
            sy = (float) (offY + scale * py);
            double target = Math.min(maxZoom, Math.max(15, zoomMilli / 1000.0));
            steps = target - zoomMilli / 1000.0 - Math.log(scale) / Math.log(2);
            if (Math.abs(sx - getWidth() / 2f) > 3 * getWidth() || Math.abs(sy - getHeight() / 2f) > 3 * getHeight()) {
                atLat = latE6;
                atLon = lonE6;
                owed = true;
                notifyAll();
                return;
            }
        }
        fly(sx, sy, steps);
    }

    /** Web Mercator world units (2^32 around) of a place in degrees times a million. */
    static long[] world(long latE6, long lonE6) {
        double lon = lonE6 / 1e6;
        double lat = Math.max(-85.0511287798, Math.min(85.0511287798, latE6 / 1e6));
        double size = 4294967296.0;
        double wx = (lon + 180.0) / 360.0 * size;
        double r = Math.toRadians(lat);
        double wy = (1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2 * size;
        return new long[] {(long) Math.floor(wx) & 0xffffffffL, (long) Math.max(0, Math.min(size - 1, Math.floor(wy)))};
    }

    /**
     * Move the screen point (sx, sy) to the centre while zooming by `steps`,
     * animated: the frame is moved and scaled as a finger would, and frames
     * are asked on the way; the last is asked at rest.
     */
    private void fly(final float sx, final float sy, final double steps) {
        if (fly != null) {
            fly.cancel();
        }
        synchronized (this) {
            flying = true;
            idle = false;
        }
        final float cx = getWidth() / 2f;
        final float cy = getHeight() / 2f;
        // Applied step by step on the screen, as a finger's gestures are,
        // so a frame that lands on the way (and is placed under the
        // screen by place()) changes nothing of the path.
        final float[] last = {sx, sy, 1f};
        fly = ValueAnimator.ofFloat(0f, 1f);
        fly.setDuration(FLY_MS);
        fly.setInterpolator(new DecelerateInterpolator());
        fly.addUpdateListener(new ValueAnimator.AnimatorUpdateListener() {
            @Override
            public void onAnimationUpdate(ValueAnimator animation) {
                float t = (float) animation.getAnimatedValue();
                float px = sx + (cx - sx) * t;
                float py = sy + (cy - sy) * t;
                float f = (float) Math.pow(2, steps * t);
                float by = f / last[2];
                synchronized (MapView.this) {
                    // Scale about where the point is, then move it on.
                    offX = by * (offX - last[0]) + px;
                    offY = by * (offY - last[1]) + py;
                    scale *= by;
                }
                last[0] = px;
                last[1] = py;
                last[2] = f;
                invalidate();
                ask();
                if (t >= 1f) {
                    synchronized (MapView.this) {
                        flying = false;
                    }
                    ask();
                }
            }
        });
        fly.start();
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
            if (fly != null && fly.isRunning()) {
                // A finger stops a fly where it is.
                fly.cancel();
            }
            synchronized (this) {
                touching = true;
                idle = false;
                flying = false;
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
        drawOwn(canvas);
        drawMarks(canvas);
    }

    /** The phone's own position (1201): an accent dot in a ring of the background. */
    private void drawOwn(Canvas canvas) {
        if (ownX == null || ownY == null || ownLat == null) {
            return;
        }
        float cx = offX + scale * ownX;
        float cy = offY + scale * ownY;
        ring.setColor(getContext().getColor(R.color.background));
        canvas.drawCircle(cx, cy, 9 * density, ring);
        circle.setColor(getContext().getColor(R.color.accent));
        canvas.drawCircle(cx, cy, 6 * density, circle);
    }

    /**
     * The people over the frame (1200): each pin the list's avatar circle
     * in a ring of the background, its age under it, faded when stale, the
     * open card's in the accent ring; a cluster a circle with its count.
     * Held by the frame's place, but never scaled: a circle stays a circle
     * of PIN_DP.
     */
    private void drawMarks(Canvas canvas) {
        float side = PIN_DP * density;
        float edge = 2 * density;
        long now = Ui.nowSecs();
        for (Map<String, Object> mark : marks) {
            float cx = offX + scale * Core.num(mark, "x");
            float cy = offY + scale * Core.num(mark, "y");
            List<String> names = strings(mark.get("peers"));
            if (names.isEmpty()) {
                continue;
            }
            if (names.size() > 1) {
                ring.setColor(getContext().getColor(R.color.background));
                canvas.drawCircle(cx, cy, side / 2f + edge, ring);
                circle.setColor(getContext().getColor(R.color.badge));
                canvas.drawCircle(cx, cy, side / 2f, circle);
                letters.setColor(getContext().getColor(R.color.badge_text));
                letters.setTextAlign(Paint.Align.CENTER);
                letters.setTypeface(Typeface.create(Typeface.DEFAULT, Typeface.BOLD));
                letters.setTextSize(side * 0.42f);
                Paint.FontMetrics m = letters.getFontMetrics();
                canvas.drawText(String.valueOf(names.size()), cx, cy - (m.ascent + m.descent) / 2f, letters);
                continue;
            }
            String peer = names.get(0);
            Map<String, Object> row = people.get(peer);
            if (row == null) {
                row = new HashMap<>();
                row.put("initials", peer.length() >= 2 ? peer.substring(0, 2).toUpperCase(java.util.Locale.ROOT) : "?");
            }
            boolean stale = Core.bool(row, "stale");
            int layer = canvas.saveLayerAlpha(cx - side, cy - side, cx + side, cy + side * 2, stale ? 128 : 255);
            ring.setColor(getContext().getColor(peer.equals(carded) ? R.color.accent : R.color.background));
            canvas.drawCircle(cx, cy, side / 2f + (peer.equals(carded) ? 2 * edge : edge), ring);
            AvatarView.drawAt(canvas, cx, cy, side, peer, row, circle, letters, picture);
            if (row.containsKey("at")) {
                String age = Ui.age(getContext(), Core.num(row, "at"), now);
                Paint.FontMetrics m = label.getFontMetrics();
                float w = label.measureText(age) + 8 * density;
                float h = m.descent - m.ascent + 2 * density;
                float top = cy + side / 2f + 3 * density;
                canvas.drawRoundRect(new RectF(cx - w / 2, top, cx + w / 2, top + h), h / 2, h / 2, labelGround);
                canvas.drawText(age, cx, top + density - m.ascent, label);
            }
            canvas.restoreToCount(layer);
        }
    }

    /** A msgpack array of strings as a list. */
    private static List<String> strings(Object value) {
        List<String> out = new ArrayList<>();
        if (value instanceof List) {
            for (Object item : (List<?>) value) {
                out.add(String.valueOf(item));
            }
        }
        return out;
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
                moving = touching || flying;
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
                        "about_x", (long) (w / 2), "about_y", (long) (h / 2),
                        "avatars", true);
                if (x != null) {
                    request.put("x", x);
                    request.put("y", y);
                    request.put("zoom_milli", zoomMilli);
                }
                if (selected != null) request.put("selected", selected);
                if (atLat != null) {
                    request.put("at_lat_e6", atLat);
                    request.put("at_lon_e6", atLon);
                    if (atZoom != null) request.put("at_zoom_milli", atZoom);
                }
                if (ownLat != null) {
                    request.put("own_lat_e6", ownLat);
                    request.put("own_lon_e6", ownLon);
                }
                atLat = null;
                atLon = null;
                atZoom = null;
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
                    marks = Core.list(shown, "marks");
                    ownX = shown.get("own_x") instanceof Number ? Core.num(shown, "own_x") : null;
                    ownY = shown.get("own_y") instanceof Number ? Core.num(shown, "own_y") : null;
                    viewW = w;
                    viewH = h;
                    maxZoom = Core.num(shown, "max_zoom");
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
        if (first || (!moving && !touching && !flying && !owed)) {
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
