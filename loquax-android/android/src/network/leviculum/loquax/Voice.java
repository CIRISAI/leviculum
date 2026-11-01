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
    /** Columba's limit, the library's LONGEST_MS: the recorder stops there. */
    static final long LONGEST_MS = 5 * 60 * 1000;
    /**
     * In a debuggable build, a file of this name in the app's files is
     * recorded instead of the microphone (16 kHz mono 16-bit): the device
     * walk's fixture, since an emulator has no voice to give.
     */
    static final String FIXTURE = "voice-fixture.pcm";

    private Voice() {}

    /** The microphone, from press to release. Not thread safe: the UI thread's. */
    static final class Recorder {
        private final Context context;
        private AudioRecord record;
        private Thread reader;
        private volatile boolean running;
        private final ByteArrayOutputStream pcm = new ByteArrayOutputStream();
        private int rate;
        private long started;

        Recorder(Context context) {
            this.context = context;
        }

        /** Starts; false when no microphone would open. */
        boolean start() {
            pcm.reset();
            started = System.currentTimeMillis();
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
            reader = new Thread(new Runnable() {
                @Override
                public void run() {
                    byte[] buffer = new byte[rate / 10 * 2];
                    while (running) {
                        int n = r.read(buffer, 0, buffer.length);
                        if (n > 0) {
                            synchronized (pcm) {
                                pcm.write(buffer, 0, n);
                            }
                        }
                    }
                }
            }, "voice-record");
            reader.start();
            return true;
        }

        private void readFixture(File fixture) {
            byte[] buffer = new byte[rate / 10 * 2];
            try (FileInputStream in = new FileInputStream(fixture)) {
                int n;
                while (running && (n = in.read(buffer)) > 0) {
                    synchronized (pcm) {
                        pcm.write(buffer, 0, n);
                    }
                    // In real time, so a hold of the button means a length.
                    Thread.sleep(100);
                }
            } catch (IOException | InterruptedException e) {
                // What was read is the recording.
            }
        }

        /** How long it has recorded. */
        long ms() {
            return System.currentTimeMillis() - started;
        }

        int rate() {
            return rate;
        }

        /** Stops; the PCM recorded, little endian. */
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
                    // Never started: nothing recorded.
                }
                record.release();
                record = null;
            }
            synchronized (pcm) {
                return pcm.toByteArray();
            }
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
}
