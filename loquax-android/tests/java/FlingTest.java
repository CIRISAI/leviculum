package network.leviculum.loquax;

import java.util.ArrayList;
import java.util.List;

/**
 * The map's fling (1188) on the host, run by tests/fling.rs: which
 * gestures fling, by a held clock, and a coast frame by frame. The
 * scroller here is a stand-in with OverScroller's contract (it starts at
 * (0, 0), slows down to a stop, ends at a y bound without passing it);
 * the phone's is the platform's own, whose curve this does not copy.
 */
final class FlingTest {
    /** ViewConfiguration's minimum fling velocity at 2.75 dp: 50 dp a second. */
    static final float MINIMUM = 50 * 2.75f;

    /** A clock the test moves. */
    static long now;

    /** Slows down as e^(-t / TAU), stops below a pixel a second or at a y bound. */
    static final class HeldScroller implements Fling.Scroller {
        static final double TAU = 0.325;
        long started;
        double vx;
        double vy;
        int minY;
        int maxY;
        int x;
        int y;
        boolean done = true;

        @Override
        public void fling(int vx, int vy, int minY, int maxY) {
            started = now;
            this.vx = vx;
            this.vy = vy;
            this.minY = minY;
            this.maxY = maxY;
            x = 0;
            y = 0;
            done = false;
        }

        @Override
        public boolean step() {
            if (done) {
                return false;
            }
            double t = (now - started) / 1000.0;
            double gone = TAU * (1 - Math.exp(-t / TAU));
            x = (int) Math.round(vx * gone);
            long wantY = Math.round(vy * gone);
            y = (int) Math.max(minY, Math.min(maxY, wantY));
            double speed = Math.hypot(vx, vy) * Math.exp(-t / TAU);
            if (speed < 1 || wantY != y) {
                done = true;
                // Where the curve ends: its whole distance, held to the bounds.
                if (wantY == y) {
                    x = (int) Math.round(vx * TAU);
                    y = (int) Math.max(minY, Math.min(maxY, Math.round(vy * TAU)));
                }
            }
            return !done;
        }

        @Override
        public int x() {
            return x;
        }

        @Override
        public int y() {
            return y;
        }

        @Override
        public void stop() {
            done = true;
        }
    }

    static int checks;

    static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }

    /** A one finger pan from down at 0 to the last move at `lastMoveMs`, lifted at `upMs`. */
    static boolean pan(long lastMoveMs, long upMs, float vx, float vy, boolean on) {
        Fling fling = new Fling();
        now = 0;
        fling.down(now);
        for (now = 16; now <= lastMoveMs; now += 16) {
            fling.panned(now);
        }
        fling.panned(lastMoveMs);
        return fling.flings(upMs, vx, vy, MINIMUM, on);
    }

    static void whichGesturesFling() {
        check(pan(160, 170, 0, 1200, true), "a fast pan flings");
        check(pan(160, 170, MINIMUM * 0.8f, MINIMUM * 0.6f, true),
                "a pan at the minimum velocity, across both axes, flings");
        check(!pan(160, 170, MINIMUM * 0.7f, MINIMUM * 0.7f, true),
                "a release below the minimum velocity does not fling");
        check(!pan(160, 170, 0, 1200, false), "map_fling off: no fling");
        check(pan(160, 160 + Fling.STOPPED_MS, 0, 1200, true),
                "lifted the stopped time after the last move: still moving");
        check(!pan(160, 161 + Fling.STOPPED_MS, 0, 1200, true),
                "a finger that rested before it lifted had stopped");

        Fling tap = new Fling();
        tap.down(0);
        check(!tap.flings(90, 3000, 0, MINIMUM, true), "a tap that never panned does not fling");

        Fling pinch = new Fling();
        pinch.down(0);
        pinch.panned(16);
        pinch.pointerDown();
        pinch.panned(32);
        pinch.panned(48);
        check(!pinch.flings(50, 4000, 0, MINIMUM, true), "a pinch never flings");

        Fling under = new Fling();
        under.down(0);
        under.pointerDown();
        under.panned(16);
        under.panned(32);
        check(!under.flings(40, 4000, 0, MINIMUM, true),
                "a pan that ended while another finger was down does not fling");

        Fling next = new Fling();
        next.down(0);
        next.pointerDown();
        next.down(500);
        next.panned(516);
        check(next.flings(520, 4000, 0, MINIMUM, true), "the next gesture starts clean");
    }

    static void theRoomOfACoast() {
        int[] room = Fling.room(0, 1f, 2400, 100, 900);
        check(room[0] == -900 && room[1] == 100, "at the last frame: " + room[0] + " " + room[1]);
        // The finger moved the map down 30 pixels: the view went 30 north.
        room = Fling.room(30, 1f, 2400, 100, 900);
        check(room[0] == -930 && room[1] == 70, "moved 30 north: " + room[0] + " " + room[1]);
        // The frame shown at twice its size about the screen's centre, the
        // view where it was: a view pixel is two on screen.
        room = Fling.room(-1200, 2f, 2400, 100, 900);
        check(room[0] == -1800 && room[1] == 200, "scaled: " + room[0] + " " + room[1]);
        // Past the edge already (a frame in flight): no step across it.
        room = Fling.room(150, 1f, 2400, 100, 900);
        check(room[0] == -1050 && room[1] == 0, "past the edge: " + room[0] + " " + room[1]);
    }

    /** The frames of a coast every 16 ms: {dx, dy} each, then whether it still ran. */
    static List<int[]> coast(Fling.Coast coast, float vx, float vy, int[] room) {
        List<int[]> frames = new ArrayList<>();
        now = 1000;
        coast.start(vx, vy, room);
        check(coast.running(), "a coast runs once started");
        while (coast.running()) {
            now += 16;
            int[] by = coast.frame();
            check(by != null, "a running coast has a frame");
            frames.add(by);
            check(frames.size() < 1000, "the coast converges");
        }
        check(coast.frame() == null, "after the last frame, none");
        return frames;
    }

    static void aCoastSlowsDownAndEndsAtRest() {
        HeldScroller scroller = new HeldScroller();
        Fling.Coast coast = new Fling.Coast(scroller);
        List<int[]> frames = coast(coast, 2400, -1800, Fling.room(0, 1f, 2400, 100_000, 100_000));
        check(frames.size() > 20, "a coast of " + frames.size() + " frames");
        double last = Double.MAX_VALUE;
        long x = 0;
        long y = 0;
        for (int[] by : frames) {
            double step = Math.hypot(by[0], by[1]);
            // Each axis rounded to whole pixels: a step may pass the one
            // before by a pixel on each.
            check(step <= last + Math.sqrt(2), "steps shrink: " + step + " after " + last);
            check(by[0] >= 0 && by[1] <= 0, "the coast keeps its heading");
            last = step;
            x += by[0];
            y += by[1];
        }
        check(last <= Math.sqrt(2), "the last step is a pixel at most: " + last);
        check(x == scroller.x() && y == scroller.y(), "the steps add up to where the scroller ended");
        check(Math.abs(x - 2400 * HeldScroller.TAU) <= 1, "converged at " + x);
        check(!coast.running(), "ended: the next frame is asked at rest");
    }

    static void aCoastStopsAtTheEdge() {
        HeldScroller scroller = new HeldScroller();
        Fling.Coast coast = new Fling.Coast(scroller);
        // 70 view pixels to the north edge, flung north at 5000 px/s.
        List<int[]> frames = coast(coast, 0, 5000, Fling.room(30, 1f, 2400, 100, 900));
        long y = 0;
        for (int[] by : frames) {
            y += by[1];
            check(y <= 70, "never past the edge: " + y);
        }
        check(y == 70, "stopped at the edge: " + y);
        check(!coast.running(), "at rest at the edge, no bounce");
    }

    static void aFingerStopsACoast() {
        HeldScroller scroller = new HeldScroller();
        Fling.Coast coast = new Fling.Coast(scroller);
        now = 0;
        coast.start(0, 3000, Fling.room(0, 1f, 2400, 100_000, 100_000));
        now = 48;
        int[] by = coast.frame();
        check(by != null && by[1] > 0, "moving");
        coast.stop();
        check(!coast.running() && scroller.done, "a finger down ends it at once");
        check(coast.frame() == null, "and it asks no frame more");
    }

    public static void main(String[] args) {
        whichGesturesFling();
        theRoomOfACoast();
        aCoastSlowsDownAndEndsAtRest();
        aCoastStopsAtTheEdge();
        aFingerStopsACoast();
        System.out.println("fling: " + checks + " checks passed");
    }
}
