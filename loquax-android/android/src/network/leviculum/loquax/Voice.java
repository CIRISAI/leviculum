package network.leviculum.loquax;

import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.View;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;

/**
 * Voice messages on the phone (1222): the microphone's PCM and the
 * player's, nothing else. Recording hands mono 16-bit PCM to the library,
 * which codes it as Opus in Ogg and sends it (`Request::Voice`); playing
 * asks the library for the sound decoded (`Query::Voice`) and writes it to
 * an AudioTrack. Codec, length rules and the bubble's words are the
 * library's (loquax/src/ui/voice.rs, loquax/src/voice.rs).
 */
final class Voice {
    /** The permission request's code. */
    static final int ASK = 0x5622;
    /**
     * In a debuggable build, a file of this name in the app's files is
     * recorded instead of the microphone (16 kHz mono 16-bit): the device
     * walk's fixture, since an emulator has no voice to give. It feeds its
     * levels as the microphone does, window by window in real time, so a
     * fixture of zeros is a microphone that hears nothing (1276).
     */
    static final String FIXTURE = "voice-fixture.pcm";

    private Voice() {}

    /**
     * The microphone, from the library's `start` to its `finish` or
     * `discard` (1276): it records, pauses and goes on as the recording's
     * effects say, and hands each window's RMS to `levels` with the
     * phone's uptime clock. How long it may run is the library's: the cap
     * comes as a `stop` effect. Its methods are the UI thread's; `levels`
     * is called on the reader thread.
     */
    static final class Recorder {
        /** A window recorded: its end on the uptime clock, and its RMS. */
        interface Feed {
            void level(long now, int rms);
        }

        private final Context context;
        private final Feed feed;
        private final int windowMs;
        private AudioRecord record;
        private Thread reader;
        private volatile boolean running;
        private volatile boolean paused;
        private final ByteArrayOutputStream pcm = new ByteArrayOutputStream();
        private int rate;

        Recorder(Context context, int windowMs, Feed feed) {
            this.context = context;
            this.windowMs = windowMs > 0 ? windowMs : 50;
            this.feed = feed;
        }

        /** Starts; false when no microphone would open. */
        boolean start() {
            pcm.reset();
            final File fixture = new File(context.getFilesDir(), FIXTURE);
            boolean debuggable =
                    (context.getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
            if (debuggable && fixture.isFile()) {
                rate = 16_000;
                running = true;
                reader = new Thread(new Runnable() {
                    @Override
                    public void run() {
                        readFixture(fixture);
                    }
                }, "voice-fixture");
                reader.start();
                return true;
            }
            // 16 kHz is what the library codes; 48 and 44.1 kHz are what
            // every phone has (44.1 the only one Android promises).
            for (int candidate : new int[] {16_000, 48_000, 44_100}) {
                int min = AudioRecord.getMinBufferSize(candidate,
                        AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
                if (min <= 0) continue;
                try {
                    AudioRecord r = new AudioRecord(MediaRecorder.AudioSource.MIC, candidate,
                            AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT,
                            Math.max(min, candidate / 5));
                    if (r.getState() != AudioRecord.STATE_INITIALIZED) {
                        r.release();
                        continue;
                    }
                    record = r;
                    rate = candidate;
                    break;
                } catch (SecurityException | IllegalArgumentException e) {
                    return false;
                }
            }
            if (record == null) return false;
            record.startRecording();
            running = true;
            final AudioRecord r = record;
            final Levels levels = levels();
            reader = new Thread(new Runnable() {
                @Override
                public void run() {
                    // One window a read: the levels come as the sound does.
                    byte[] buffer = new byte[levels.samples() * 2];
                    while (running) {
                        if (paused) {
                            pause(10);
                            continue;
                        }
                        // A read the pause cut short is sound from before
                        // it: kept, and its level the library drops.
                        int n = r.read(buffer, 0, buffer.length);
                        if (n > 0) {
                            synchronized (pcm) {
                                pcm.write(buffer, 0, n);
                            }
                            levels.feed(buffer, 0, n);
                        } else if (n <= 0) {
                            pause(10);
                        }
                    }
                }
            }, "voice-record");
            reader.start();
            return true;
        }

        /** The window cutter, its windows to `feed` at the time each ends. */
        private Levels levels() {
            return new Levels(rate, windowMs, new Levels.Sink() {
                @Override
                public void window(int rms) {
                    feed.level(SystemClock.uptimeMillis(), rms);
                }
            });
        }

        private void readFixture(File fixture) {
            Levels levels = levels();
            byte[] buffer = new byte[levels.samples() * 2];
            try (FileInputStream in = new FileInputStream(fixture)) {
                int n;
                while (running) {
                    if (paused) {
                        Thread.sleep(10);
                        continue;
                    }
                    if ((n = in.read(buffer)) <= 0) break;
                    synchronized (pcm) {
                        pcm.write(buffer, 0, n);
                    }
                    // In real time, so a hold of the button means a length.
                    Thread.sleep(windowMs);
                    levels.feed(buffer, 0, n);
                }
            } catch (IOException | InterruptedException e) {
                // What was read is the recording.
            }
        }

        private static void pause(long ms) {
            try {
                Thread.sleep(ms);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }

        int rate() {
            return rate;
        }

        /** The library's `pause`, and its `stop` at the cap: what is there stays. */
        void pause() {
            paused = true;
            if (record != null) {
                try {
                    record.stop();
                } catch (IllegalStateException e) {
                    // Not recording: nothing to stop.
                }
            }
        }

        /** The library's `resume`: on with the same recording. */
        void resume() {
            if (record != null) {
                try {
                    record.startRecording();
                } catch (IllegalStateException e) {
                    // Released: nothing goes on.
                    return;
                }
            }
            paused = false;
        }

        /** What is recorded so far, little endian: the preview's sound. */
        byte[] pcm() {
            synchronized (pcm) {
                return pcm.toByteArray();
            }
        }

        /** Stops for good; the PCM recorded, little endian. */
        byte[] stop() {
            running = false;
            if (reader != null) {
                try {
                    reader.join(500);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
            if (record != null) {
                try {
                    record.stop();
                } catch (IllegalStateException e) {
                    // Never started or paused: nothing recorded since.
                }
                record.release();
                record = null;
            }
            return pcm();
        }
    }

    /** What the player tells the chat. On the UI thread. */
    interface Listener {
        void progress(long key, float done);

        void ended(long key);
    }

    /** One voice message at a time, through an AudioTrack. */
    static final class Player {
        private final Handler ui = new Handler(Looper.getMainLooper());
        private volatile AudioTrack track;
        private volatile long playing = Long.MIN_VALUE;
        private volatile boolean stopped;

        /** The key of the one playing, Long.MIN_VALUE for none. */
        long playing() {
            return playing;
        }

        /** Plays `pcm` (mono 16-bit little endian at `rate`); not on the UI thread. */
        void play(final long key, byte[] pcm, int rate, final Listener listener) {
            stop();
            stopped = false;
            int min = AudioTrack.getMinBufferSize(rate, AudioFormat.CHANNEL_OUT_MONO,
                    AudioFormat.ENCODING_PCM_16BIT);
            AudioTrack t = new AudioTrack.Builder()
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                            .build())
                    .setAudioFormat(new AudioFormat.Builder()
                            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                            .setSampleRate(rate)
                            .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                            .build())
                    .setBufferSizeInBytes(Math.max(min, rate / 5 * 2))
                    .setTransferMode(AudioTrack.MODE_STREAM)
                    .build();
            track = t;
            playing = key;
            t.play();
            int chunk = rate / 10 * 2;
            for (int at = 0; at < pcm.length && !stopped; at += chunk) {
                int n = Math.min(chunk, pcm.length - at);
                t.write(pcm, at, n);
                final float done = (at + n) / (float) pcm.length;
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        listener.progress(key, done);
                    }
                });
            }
            if (!stopped) {
                // Let the last buffer sound out before saying the end.
                try {
                    Thread.sleep(200);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
            t.release();
            if (track == t) track = null;
            if (playing == key) playing = Long.MIN_VALUE;
            ui.post(new Runnable() {
                @Override
                public void run() {
                    listener.ended(key);
                }
            });
        }

        void stop() {
            stopped = true;
            AudioTrack t = track;
            if (t != null) {
                try {
                    t.pause();
                    t.flush();
                } catch (IllegalStateException e) {
                    // Released by the playing thread already.
                }
            }
        }
    }

    /**
     * The level bar (1222): the library's heights, 0 to 100, as rounded
     * bars; played in the bright colour up to the player's position, the
     * rest dimmed. An even bar for a recording without heights (Codec2).
     */
    static final class Bar extends View {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private byte[] levels = new byte[0];
        private float done;
        private int color;

        Bar(Context context) {
            super(context);
        }

        void set(byte[] levels, int color, float done) {
            this.levels = levels == null ? new byte[0] : levels;
            this.color = color;
            this.done = done;
            invalidate();
        }

        void progress(float done) {
            this.done = done;
            invalidate();
        }

        @Override
        protected void onDraw(Canvas canvas) {
            int bars = levels.length == 0 ? 32 : levels.length;
            float w = getWidth() / (float) bars;
            float h = getHeight();
            for (int i = 0; i < bars; i++) {
                int level = levels.length == 0 ? 40 : (levels[i] & 0xff);
                float bar = Math.max(h * 0.12f, h * level / 100f);
                paint.setColor(color);
                paint.setAlpha((i + 0.5f) / bars <= done ? 255 : 110);
                float x = i * w + w * 0.2f;
                canvas.drawRoundRect(x, (h - bar) / 2, x + w * 0.6f, (h + bar) / 2,
                        w * 0.3f, w * 0.3f, paint);
            }
        }
    }

    /**
     * The recording bar's waveform (1276): the library's levels, 0 to 100,
     * as bars in the accent colour, loud ones tall and bright, quiet ones
     * short and dim. While it records (`moving`) the bars stand at a fixed
     * pitch of `slots` with the newest at the right edge, so the picture
     * scrolls right to left as levels come; standing still it is the whole
     * recording across the width. It sends no accessibility events of its
     * own as it redraws: its words are its content description.
     */
    static final class Wave extends View {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private byte[] levels = new byte[0];
        private boolean moving;
        private int slots = 60;
        private int color;

        Wave(Context context) {
            super(context);
        }

        void set(byte[] levels, boolean moving, int slots, int color) {
            this.levels = levels == null ? new byte[0] : levels;
            this.moving = moving;
            this.slots = Math.max(1, slots);
            this.color = color;
            invalidate();
        }

        @Override
        protected void onDraw(Canvas canvas) {
            int n = levels.length;
            if (n == 0) return;
            int columns = moving ? Math.max(slots, n) : n;
            float w = getWidth() / (float) columns;
            float h = getHeight();
            int first = columns - n;
            paint.setColor(color);
            for (int i = 0; i < n; i++) {
                int level = Math.min(100, levels[i] & 0xff);
                float bar = Math.max(h * 0.08f, h * level / 100f);
                paint.setAlpha(90 + level * 165 / 100);
                float x = (first + i) * w + w * 0.18f;
                canvas.drawRoundRect(x, (h - bar) / 2, x + w * 0.64f, (h + bar) / 2,
                        w * 0.32f, w * 0.32f, paint);
            }
        }
    }

    /**
     * The red dot of the recording bar (1276): pulsing while it records and
     * is heard, steady once it stands or hears nothing. Drawn, not
     * animated through the view's alpha, so a pulse is no accessibility
     * event; `selected` says whether it pulses, for the device walk.
     */
    static final class Dot extends View {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private boolean pulsing;
        private int color;

        Dot(Context context) {
            super(context);
        }

        void set(boolean pulsing, int color) {
            this.color = color;
            if (this.pulsing != pulsing) {
                this.pulsing = pulsing;
                setSelected(pulsing);
            }
            invalidate();
        }

        @Override
        protected void onDraw(Canvas canvas) {
            float r = Math.min(getWidth(), getHeight()) / 2f;
            int alpha = 255;
            if (pulsing) {
                double t = (SystemClock.uptimeMillis() % 1200) / 1200.0;
                alpha = (int) (110 + 145 * (0.5 + 0.5 * Math.cos(2 * Math.PI * t)));
                postInvalidateOnAnimation();
            }
            paint.setColor(color);
            paint.setAlpha(alpha);
            canvas.drawCircle(getWidth() / 2f, getHeight() / 2f, r * 0.8f, paint);
        }
    }
}
