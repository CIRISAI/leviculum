package network.leviculum.loquax;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.RectF;
import android.util.Log;
import android.view.Choreographer;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.ScaleGestureDetector;
import android.view.VelocityTracker;
import android.view.View;
import android.view.ViewConfiguration;
import android.widget.OverScroller;

import java.util.Locale;
import java.util.Map;

/**
 * The photo whole, zoomed (1269): this view only reports what the
 * fingers do and draws the picture where the library's zoom
 * (loquax::ui::zoom, through {@link Core#zoom}) puts it, the window's
 * state machine too. A pinch zooms about its centre, one finger pans
 * while zoomed, a double tap toggles 1x and 2x at the tap, a fling while
 * zoomed coasts and stops at the edges: which gesture flings is
 * {@link Fling}'s, as on the map, the physics an OverScroller's bounded
 * by the zoom's room. At 1x a tap or a swipe down closes.
 *
 * Each gesture's result is logged once as
 * `PHOTO_ZOOM scale=.. left=.. top=.. width=.. height=.. view=WxH` under
 * the tag `loquax`, which the device walk's `picture_zoom` reads.
 */
final class PhotoView extends View {
    /** What the viewer asks of its screen. */
    interface Listener {
        void close();
    }

    static final String TAG = "loquax";

    private final Listener listener;
    private final Paint paint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final RectF at = new RectF();
    private final GestureDetector gestures;
    private final ScaleGestureDetector pinch;
    private final Fling fling = new Fling();
    private final Fling.Coast coast;
    private final OverScroller scroller;
    private VelocityTracker velocity;
    private final float minFling;
    private final float maxFling;
    /** The coast's x bounds, set before it starts: x is bounded too here. */
    private int coastMinX;
    private int coastMaxX;
    private final Choreographer.FrameCallback coastFrame = new Choreographer.FrameCallback() {
        @Override
        public void doFrame(long frameTimeNanos) {
            int[] by = coast.frame();
            if (by == null) {
                return;
            }
            Map<String, Object> next = Core.zoom(state, "pan", "dx", by[0], "dy", by[1]);
            boolean moving = coast.running();
            if (Core.bool(next, "ok")) {
                state = next;
                invalidate();
                // An edge stopped it on both axes: the coast is over.
                if (Core.real(next, "moved_x") == 0 && Core.real(next, "moved_y") == 0
                        && (by[0] != 0 || by[1] != 0)) {
                    coast.stop();
                    moving = false;
                }
            }
            if (moving) {
                Choreographer.getInstance().postFrameCallback(this);
            } else {
                logState();
            }
        }
    };

    private Bitmap bitmap;
    /** The zoom's state, as the library last answered it. */
    private Map<String, Object> state = Core.map();
    /** Where the gesture's first finger went down, for the swipe down. */
    private float downX;
    private float downY;
    /** The gesture had a second finger or zoomed: it is no swipe. */
    private boolean many;

    PhotoView(Context context, Listener listener) {
        super(context);
        this.listener = listener;
        setBackgroundColor(Color.BLACK);
        ViewConfiguration platform = ViewConfiguration.get(context);
        minFling = platform.getScaledMinimumFlingVelocity();
        maxFling = platform.getScaledMaximumFlingVelocity();
        scroller = new OverScroller(context);
        coast = new Fling.Coast(new Fling.Scroller() {
            @Override
            public void fling(int vx, int vy, int minY, int maxY) {
                scroller.fling(0, 0, vx, vy, coastMinX, coastMaxX, minY, maxY);
            }

            @Override
            public boolean step() {
                // The step that ends a fling still moves it to its end.
                return scroller.computeScrollOffset() && !scroller.isFinished();
            }

            @Override
            public int x() {
                return scroller.getCurrX();
            }

            @Override
            public int y() {
                return scroller.getCurrY();
            }

            @Override
            public void stop() {
                scroller.forceFinished(true);
            }
        });
        gestures = new GestureDetector(context, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                return true;
            }

            @Override
            public boolean onScroll(MotionEvent a, MotionEvent b, float dx, float dy) {
                if (pinch.isInProgress() || Core.bool(state, "at_rest")) {
                    return true;
                }
                fling.panned(b.getEventTime());
                apply(Core.zoom(state, "pan", "dx", -dx, "dy", -dy));
                return true;
            }

            @Override
            public boolean onSingleTapConfirmed(MotionEvent e) {
                if (Core.bool(state, "at_rest")) {
                    PhotoView.this.listener.close();
                }
                return true;
            }

            @Override
            public boolean onDoubleTap(MotionEvent e) {
                apply(Core.zoom(state, "doubletap", "x", e.getX(), "y", e.getY()));
                logState();
                return true;
            }
        });
        pinch = new ScaleGestureDetector(context, new ScaleGestureDetector.SimpleOnScaleGestureListener() {
            @Override
            public boolean onScale(ScaleGestureDetector detector) {
                apply(Core.zoom(state, "pinch", "factor", detector.getScaleFactor(),
                        "x", detector.getFocusX(), "y", detector.getFocusY()));
                return true;
            }
        });
        // The double tap is the zoom's; the platform's own quick scale
        // (double tap and drag) would zoom a second way.
        pinch.setQuickScaleEnabled(false);
    }

    /** The picture to show, `fullW` by `fullH` its own size. */
    void show(Bitmap bitmap, int fullW, int fullH) {
        this.bitmap = bitmap;
        state = Core.map("view_w", (double) Math.max(1, getWidth()),
                "view_h", (double) Math.max(1, getHeight()),
                "picture_w", (double) fullW, "picture_h", (double) fullH,
                "scale", 1.0, "pan_x", 0.0, "pan_y", 0.0);
        apply(Core.zoom(state, "none"));
        logState();
    }

    /** Back: zoomed, 1x again; whether it was at 1x, so the screen closes. */
    boolean back() {
        stopCoast();
        Map<String, Object> next = Core.zoom(state, "back");
        apply(next);
        logState();
        return !Core.bool(next, "ok") || Core.bool(next, "close");
    }

    private void apply(Map<String, Object> next) {
        if (!Core.bool(next, "ok")) {
            Log.w(TAG, "zoom: " + Core.str(next, "error"));
            return;
        }
        state = next;
        invalidate();
    }

    private void logState() {
        if (bitmap == null) {
            return;
        }
        Log.i(TAG, String.format(Locale.ROOT,
                "PHOTO_ZOOM scale=%.3f left=%.1f top=%.1f width=%.1f height=%.1f view=%dx%d",
                Core.real(state, "scale"), Core.real(state, "left"), Core.real(state, "top"),
                Core.real(state, "width"), Core.real(state, "height"), getWidth(), getHeight()));
    }

    private void stopCoast() {
        coast.stop();
        Choreographer.getInstance().removeFrameCallback(coastFrame);
    }

    @Override
    protected void onSizeChanged(int w, int h, int oldw, int oldh) {
        super.onSizeChanged(w, h, oldw, oldh);
        if (bitmap == null) {
            return;
        }
        state.put("view_w", (double) w);
        state.put("view_h", (double) h);
        apply(Core.zoom(state, "resize"));
        logState();
    }

    @Override
    protected void onDetachedFromWindow() {
        stopCoast();
        super.onDetachedFromWindow();
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        if (bitmap == null || !Core.bool(state, "ok")) {
            return;
        }
        float left = (float) Core.real(state, "left");
        float top = (float) Core.real(state, "top");
        at.set(left, top, left + (float) Core.real(state, "width"),
                top + (float) Core.real(state, "height"));
        canvas.drawBitmap(bitmap, null, at, paint);
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        if (bitmap == null) {
            return true;
        }
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN) {
            // A finger stops a coast where it is and takes over.
            stopCoast();
            fling.down(event.getEventTime());
            downX = event.getX();
            downY = event.getY();
            many = !Core.bool(state, "at_rest");
            if (velocity == null) {
                velocity = VelocityTracker.obtain();
            }
            velocity.clear();
        } else if (action == MotionEvent.ACTION_POINTER_DOWN) {
            fling.pointerDown();
            many = true;
        }
        if (velocity != null) {
            velocity.addMovement(event);
        }
        pinch.onTouchEvent(event);
        gestures.onTouchEvent(event);
        if (action == MotionEvent.ACTION_UP) {
            if (!many) {
                Map<String, Object> swipe = Core.zoom(state, "swipe",
                        "dx", event.getX() - downX, "dy", event.getY() - downY);
                if (Core.bool(swipe, "close")) {
                    listener.close();
                    return true;
                }
            }
            if (!startCoast(event)) {
                logState();
            }
        } else if (action == MotionEvent.ACTION_CANCEL) {
            logState();
        }
        return true;
    }

    /**
     * The last finger lifted (`up`): start a coast when the gesture
     * flings, bounded by the zoom's room to each edge.
     */
    private boolean startCoast(MotionEvent up) {
        if (velocity == null || Core.bool(state, "at_rest")) {
            return false;
        }
        velocity.computeCurrentVelocity(1000, maxFling);
        int id = up.getPointerId(up.getActionIndex());
        float vx = velocity.getXVelocity(id);
        float vy = velocity.getYVelocity(id);
        if (!fling.flings(up.getEventTime(), vx, vy, minFling, true)) {
            return false;
        }
        coastMinX = (int) -Math.floor(Core.real(state, "room_left"));
        coastMaxX = (int) Math.floor(Core.real(state, "room_right"));
        int[] room = {(int) -Math.floor(Core.real(state, "room_up")),
                (int) Math.floor(Core.real(state, "room_down"))};
        coast.start(vx, vy, room);
        Choreographer.getInstance().postFrameCallback(coastFrame);
        return true;
    }
}
