package network.leviculum.loquax;

/**
 * The map's fling (1188): whether a finger that lets go of a pan sets the
 * map coasting, and the coast itself, frame by frame. Plain Java, no
 * Android class, so the host's tests run it (tests/fling.rs): the physics
 * are the platform's, an OverScroller behind {@link Scroller} on the
 * phone (MapView), whose fling is ListView's and ScrollView's, with
 * ViewConfiguration's scroll friction and the screen's density; a test
 * puts a scroller of its own behind it.
 *
 * A gesture flings when it was a pan of one finger from down to up, the
 * finger still moving when it lifted, at the platform's minimum fling
 * velocity at least, and the config's `map_fling` is on. A second finger
 * at any time makes it a pinch, or a pan that ends under another finger:
 * neither flings.
 */
final class Fling {
    /**
     * A finger that last moved this long before it lifted had stopped:
     * AOSP's VelocityTracker assumes so since Android 13
     * (ASSUME_POINTER_STOPPED_TIME, 40 ms), but before it reports the
     * velocity of the moves before the pause, and a pan that rests and
     * then lifts would fling there.
     */
    static final long STOPPED_MS = 40;

    /** The platform's scroller, as the coast drives it. */
    interface Scroller {
        /**
         * Start a fling from (0, 0) at (vx, vy) pixels a second, y held to
         * [minY, maxY], x free; it stops at a bound, no bounce.
         */
        void fling(int vx, int vy, int minY, int maxY);

        /** Advance to now; false once the fling is over. */
        boolean step();

        int x();

        int y();

        /** End the fling where it is. */
        void stop();
    }

    private boolean panned;
    private boolean many;
    private long movedAt;

    /** The first finger came down: a new gesture. */
    void down(long nowMs) {
        panned = false;
        many = false;
        movedAt = nowMs;
    }

    /** Another finger came down: this gesture never flings. */
    void pointerDown() {
        many = true;
    }

    /** The finger panned the map. */
    void panned(long nowMs) {
        panned = true;
        movedAt = nowMs;
    }

    /**
     * The last finger lifted at `nowMs` moving at (vx, vy) pixels a
     * second, as VelocityTracker measured it: whether the map coasts on.
     * `minimum` is ViewConfiguration's minimum fling velocity, `on` the
     * config's `map_fling`.
     */
    boolean flings(long nowMs, float vx, float vy, float minimum, boolean on) {
        if (!on || many || !panned || nowMs - movedAt > STOPPED_MS) {
            return false;
        }
        return Math.hypot(vx, vy) >= minimum;
    }

    /**
     * The bounds of a coast's y, in screen pixels the map moves down: the
     * view's centre may move `north` view pixels north and `south` south
     * of the last frame's (its `room_north` and `room_south`), the screen
     * shows that frame at `scale` with its top left at offY from where it
     * was drawn, on a screen `h` high. {min, max}, 0 within both: a coast
     * that starts at the edge does not move across it.
     */
    static int[] room(float offY, float scale, int h, long north, long south) {
        float cy = h / 2f;
        // How far the view has moved south of the frame's, in its pixels.
        double moved = (cy - offY) / scale - cy;
        long max = (long) Math.floor((north + moved) * scale);
        long min = (long) -Math.floor((south - moved) * scale);
        return new int[] {(int) Math.min(0, Math.max(Integer.MIN_VALUE, min)),
                (int) Math.max(0, Math.min(Integer.MAX_VALUE, max))};
    }

    /** One coast: the scroller's offsets handed out frame by frame. */
    static final class Coast {
        private final Scroller scroller;
        private boolean running;
        private int lastX;
        private int lastY;

        Coast(Scroller scroller) {
            this.scroller = scroller;
        }

        void start(float vx, float vy, int[] room) {
            lastX = 0;
            lastY = 0;
            running = true;
            scroller.fling(Math.round(vx), Math.round(vy), room[0], room[1]);
        }

        /** A coast is on: the frames asked are moving ones. */
        boolean running() {
            return running;
        }

        /**
         * One frame of the coast: {dx, dy}, how far the map moves on
         * screen since the last; null when no coast is on. After the
         * frame that ends it, running() is false and the view is to be
         * drawn at rest.
         */
        int[] frame() {
            if (!running) {
                return null;
            }
            running = scroller.step();
            int x = scroller.x();
            int y = scroller.y();
            int[] by = {x - lastX, y - lastY};
            lastX = x;
            lastY = y;
            return by;
        }

        /** A finger came down: the coast ends where it is. */
        void stop() {
            if (running) {
                scroller.stop();
                running = false;
            }
        }
    }
}
