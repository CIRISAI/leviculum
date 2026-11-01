package network.leviculum.loquax;

/**
 * The recorder's level feed (1276), plain Java so the host runs it
 * (tests/levels.rs with tests/java/LevelsTest.java): the PCM the
 * microphone gives, cut into windows of the library's WINDOW_MS (50 ms),
 * and each window's RMS as loquax::ui::recording::rms computes it, the
 * integer square root of the integer mean of the squares. The library
 * turns each into the bar's level (`Input::Level`); a window of digital
 * silence is RMS 0, under the floor, which is how the bar learns that
 * nothing is heard.
 */
final class Levels {
    /** Where a finished window goes: its RMS. On the thread that feeds. */
    interface Sink {
        void window(int rms);
    }

    private final int samples;
    private final Sink sink;
    private long sum;
    private int count;
    /** A sample's first byte, when a feed ended between its two. */
    private int low = -1;

    /**
     * Windows of `windowMs` at `rate`, at least one sample each, cut as
     * LevelTrack::from_pcm cuts them: `rate / (1000 / windowMs)`.
     */
    Levels(int rate, int windowMs, Sink sink) {
        this.samples = Math.max(1, rate / Math.max(1, 1000 / Math.max(1, windowMs)));
        this.sink = sink;
    }

    /** The samples of one window. */
    int samples() {
        return samples;
    }

    /** `n` bytes of mono 16-bit little endian PCM from `pcm[at]`. */
    void feed(byte[] pcm, int at, int n) {
        for (int i = at; i < at + n; i++) {
            if (low < 0) {
                low = pcm[i] & 0xff;
                continue;
            }
            int sample = (short) ((pcm[i] << 8) | low);
            low = -1;
            sum += (long) sample * sample;
            count++;
            if (count == samples) {
                sink.window(rms(sum, count));
                sum = 0;
                count = 0;
            }
        }
    }

    /** The RMS of `count` samples whose squares sum to `sum`, as the library's. */
    static int rms(long sum, int count) {
        if (count == 0) {
            return 0;
        }
        long mean = sum / count;
        long root = (long) Math.sqrt((double) mean);
        while (root * root > mean) {
            root--;
        }
        while ((root + 1) * (root + 1) <= mean) {
            root++;
        }
        return (int) Math.min(root, 65535);
    }
}
