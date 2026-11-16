package network.leviculum.loquax;

import android.graphics.Bitmap;

/**
 * The Rust library, libloquax_android.so (the crate in this directory).
 * Every method here is implemented in loquax-android/src/lib.rs; the bytes
 * are msgpack in the schema of src/schema.rs, which {@link Core} reads.
 */
final class Native {
    static {
        System.loadLibrary("loquax_android");
    }

    private Native() {}

    /** Start the session; blocks while a first start joins. */
    static native byte[] start(byte[] request);

    /** One thing the user did. */
    static native byte[] command(byte[] request);

    /** What a screen shows. */
    static native byte[] query(byte[] request);

    /** The next event, or null when none came within waitMs. */
    static native byte[] nextEvent(int waitMs);

    /**
     * One frame of the map drawn straight into `into`, an ARGB_8888 bitmap
     * of the frame's size that nothing shows while the call runs; its
     * description back. `into` is null for a prefetch. Not on the UI
     * thread.
     */
    static native byte[] renderMapInto(byte[] request, Bitmap into);

    /**
     * The first QR code in a camera frame's luminance plane, `height` rows
     * of `stride` bytes; empty for none. Not on the UI thread.
     */
    static native String scan(byte[] luma, int width, int height, int stride);

    /** Stop the session. */
    static native void stop();
}
