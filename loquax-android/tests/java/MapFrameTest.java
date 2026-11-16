package network.leviculum.loquax;

/**
 * The map frame's size and the pinch's floor (1796), on the host, run by
 * tests/fling.rs. The phone of 2026-11-13: a screen of 1224 x 2197, a
 * flat frame with its margins, then the globe, which turns and asks none;
 * the Bitmap of the flat frame does not take the globe's frame and a new
 * one is made, the turned-back flat frame again. A pinch out stops where
 * the world fills the screen, from any scale, and leaves a pinch in and a
 * core without the floor alone.
 */
final class MapFrameTest {
    private static int checks;

    public static void main(String[] args) {
        int w = 1224;
        int h = 2197;
        int flatW = w + 2 * MapFrame.margin(w, false);
        int flatH = h + 2 * MapFrame.margin(h, false);
        check(flatW == 1836 && flatH == 3295, "the flat frame " + flatW + "x" + flatH);
        int globeW = w + 2 * MapFrame.margin(w, true);
        int globeH = h + 2 * MapFrame.margin(h, true);
        check(globeW == w && globeH == h, "the turning globe's frame is the screen");
        check(!MapFrame.fits(flatW, flatH, globeW, globeH), "the flat Bitmap is not the globe's");
        check(MapFrame.fits(globeW, globeH, globeW, globeH), "the globe's Bitmap is reused");
        check(!MapFrame.fits(globeW, globeH, flatW, flatH), "turned back, a new one");
        // A rotation: the same margins rule, the sides swapped.
        int turnedW = h + 2 * MapFrame.margin(h, false);
        int turnedH = w + 2 * MapFrame.margin(w, false);
        check(!MapFrame.fits(flatW, flatH, turnedW, turnedH), "a rotation makes a new Bitmap");

        // The floor: the frame at z5, the world filling the screen at
        // z3.910. A pinch to a tenth stops at the floor's scale.
        long zoom = 5000;
        long floor = 3910;
        double least = Math.pow(2, (floor - zoom) / 1000.0);
        float scale = 1f;
        scale *= MapFrame.zoomBy(scale, 0.1f, zoom, floor);
        check(Math.abs(scale - least) < 1e-6, "stopped at " + scale + ", the floor " + least);
        float again = MapFrame.zoomBy(scale, 0.5f, zoom, floor);
        check(Math.abs(scale * again - least) < 1e-6, "a further pinch out changes nothing: " + again);
        check(MapFrame.zoomBy(scale, 2f, zoom, floor) == 2f, "a pinch in is the fingers'");
        check(MapFrame.zoomBy(1f, 0.9f, zoom, floor) == 0.9f, "above the floor, the fingers'");
        check(MapFrame.zoomBy(1f, 0.01f, zoom, 0) == 0.01f, "no floor from an older core");
        // At the floor already, a frame there: nothing smaller.
        float at = MapFrame.zoomBy(1f, 0.8f, floor, floor);
        check(Math.abs(at - 1f) < 1e-6, "at the floor a pinch out holds: " + at);

        System.out.println(checks + " checks passed");
    }

    private static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
        checks++;
    }
}
