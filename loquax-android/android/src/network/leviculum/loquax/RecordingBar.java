package network.leviculum.loquax;

import android.app.Activity;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.view.Gravity;
import android.view.HapticFeedbackConstants;
import android.view.MotionEvent;
import android.view.View;
import android.view.accessibility.AccessibilityEvent;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Recording a voice message that looks like recording (1276,
 * docs/src/concepts/loquax-voice-recording.md): the compose bar gives way
 * to the recording bar the moment a finger lands on the microphone. Every
 * decision is the library's (loquax::ui::recording): this class hands it
 * the gestures (`Core.record`: press, slide in dp, release, cancel), the
 * recorder's level every 50 ms and the clock, and the bar's buttons
 * (`Core.recordingVerb`), and draws what `Core.recording` answers: the
 * pulsing dot and `REC 00:07` (amber near the cap), the live waveform,
 * "Slide left to cancel" following the finger, the held microphone grown
 * red, the lock chip rising with the swipe, the bin, pause, resume, play
 * and Send, and the line when nothing is heard or the cap is reached. It
 * runs the answer's effects: the recorder's start, pause, resume, stop,
 * finish (the PCM to `Core.voice`) and discard, the preview's play and
 * stop, the haptic ticks, and the way to the microphone's permission.
 * The UI thread's.
 */
final class RecordingBar {
    /** What the chat does for the bar. */
    interface Host {
        String peer();

        /** The line under the bar. */
        void line(String text);

        /** Whether the microphone may record; asks for it when not. */
        boolean mayRecord();

        /** A press stops the bubble that plays. */
        void stopPlaying();

        /** The recording, handed over: `Core.voice`, off the UI thread. */
        void send(byte[] pcm, int rate);

        /** The silence line's button: the permission, or the app's settings. */
        void askMicrophone();
    }

    /**
     * Drops the content-change events of a view that changes all the time
     * while it records (the timer every second): a stream of them never
     * lets the screen be idle for the device walk's dump, and a screen
     * reader reads the view's words when it gets there.
     */
    private static final View.AccessibilityDelegate QUIET = new View.AccessibilityDelegate() {
        @Override
        public void sendAccessibilityEventUnchecked(View host, AccessibilityEvent event) {
            if (event.getEventType() == AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED) {
                return;
            }
            super.sendAccessibilityEventUnchecked(host, event);
        }
    };

    /** How often the clock goes in while it records and no window came. */
    private static final long TICK_MS = 250;

    private final Activity screen;
    private final Host host;
    private final RecordFrame frame;
    private final View compose;
    private final float density;
    private final boolean debuggable;
    private final Handler ui = new Handler(Looper.getMainLooper());
    private final ExecutorService preview = Executors.newSingleThreadExecutor();
    private final Voice.Player player = new Voice.Player();

    private final FrameLayout chipArea;
    private final ImageButton chip;
    private final LinearLayout bar;
    private final Voice.Dot dot;
    private final TextView timer;
    private final Voice.Wave wave;
    private final TextView cancel;
    private final LinearLayout buttons;
    private final FrameLayout round;
    private final ImageView held;
    private final ImageButton send;
    private final LinearLayout noticeRow;
    private final TextView notice;
    private final Button allow;
    private final Map<String, View> verbs = new HashMap<>();

    private Voice.Recorder recorder;
    /** Where the finger landed, screen pixels, and the slide last said, dp. */
    private float fromX;
    private float fromY;
    private int saidX;
    private int saidY;
    /** What the walk's log line said last. */
    private String logged = "";
    private boolean moving;
    private int chipRise;
    private int cancelDp = 96;

    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            if (!moving) return;
            input("tick", SystemClock.uptimeMillis(), 0, 0, 0);
            ui.postDelayed(this, TICK_MS);
        }
    };

    RecordingBar(Activity screen, RecordFrame frame, View compose, View microphone, Host host) {
        this.screen = screen;
        this.frame = frame;
        this.compose = compose;
        this.host = host;
        this.density = screen.getResources().getDisplayMetrics().density;
        this.debuggable = (screen.getApplicationInfo().flags
                & android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0;
        int text = screen.getColor(R.color.text);
        int secondary = screen.getColor(R.color.text_secondary);
        int red = screen.getColor(R.color.rec_red);

        // Above the round button: the lock chip, with room to rise.
        chipArea = new FrameLayout(screen);
        chipArea.setVisibility(View.GONE);
        chipArea.setClipChildren(false);
        chipRise = dp(24);
        chip = new ImageButton(screen);
        chip.setId(R.id.rec_lock);
        chip.setBackground(screen.getDrawable(R.drawable.chip));
        chip.setPadding(dp(8), dp(6), dp(8), dp(6));
        chip.setScaleType(ImageView.ScaleType.FIT_CENTER);
        chip.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("rec_lock");
            }
        });
        FrameLayout.LayoutParams chipAt = new FrameLayout.LayoutParams(dp(40), dp(52),
                Gravity.END | Gravity.BOTTOM);
        chipAt.setMarginEnd(dp(8) + (dp(56) - dp(40)) / 2);
        chipAt.bottomMargin = dp(4);
        chipArea.addView(chip, chipAt);

        // The bar: a pill with the dot, the timer, the wave, the hint or
        // the buttons; the round button beside it.
        bar = new LinearLayout(screen);
        bar.setId(R.id.rec_bar);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(8), dp(4), dp(8), dp(8));
        bar.setVisibility(View.GONE);
        LinearLayout pill = new LinearLayout(screen);
        pill.setOrientation(LinearLayout.HORIZONTAL);
        pill.setGravity(Gravity.CENTER_VERTICAL);
        pill.setBackground(screen.getDrawable(R.drawable.compose_pill));
        pill.setMinimumHeight(dp(48));
        pill.setPadding(dp(12), 0, dp(4), 0);
        dot = new Voice.Dot(screen);
        dot.setId(R.id.rec_dot);
        dot.setAccessibilityDelegate(QUIET);
        pill.addView(dot, new LinearLayout.LayoutParams(dp(12), dp(12)));
        timer = new TextView(screen);
        timer.setId(R.id.rec_timer);
        timer.setTextSize(20);
        timer.setTypeface(Typeface.create(Typeface.MONOSPACE, Typeface.BOLD));
        timer.setTextColor(text);
        timer.setSingleLine(true);
        timer.setAccessibilityDelegate(QUIET);
        LinearLayout.LayoutParams timerAt = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        timerAt.setMarginStart(dp(8));
        timerAt.setMarginEnd(dp(8));
        pill.addView(timer, timerAt);
        wave = new Voice.Wave(screen);
        wave.setId(R.id.rec_wave);
        wave.setAccessibilityDelegate(QUIET);
        pill.addView(wave, new LinearLayout.LayoutParams(0, dp(32), 1));
        cancel = new TextView(screen);
        cancel.setId(R.id.rec_cancel);
        cancel.setTextSize(14);
        cancel.setTextColor(secondary);
        cancel.setSingleLine(true);
        LinearLayout.LayoutParams cancelAt = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        cancelAt.setMarginStart(dp(8));
        cancelAt.setMarginEnd(dp(8));
        pill.addView(cancel, cancelAt);
        buttons = new LinearLayout(screen);
        buttons.setOrientation(LinearLayout.HORIZONTAL);
        buttons.setGravity(Gravity.CENTER_VERTICAL);
        pill.addView(buttons, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        bar.addView(pill, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        verbs.put("rec_discard", button(R.id.rec_discard, Glyph.Kind.BIN, secondary));
        verbs.put("rec_pause", button(R.id.rec_pause, Glyph.Kind.PAUSE, text));
        verbs.put("rec_resume", button(R.id.rec_resume, Glyph.Kind.MIC, red));
        verbs.put("rec_play", button(R.id.rec_play, Glyph.Kind.PLAY, text));
        verbs.put("rec_stop_play", button(R.id.rec_stop_play, Glyph.Kind.STOP, text));

        round = new FrameLayout(screen);
        LinearLayout.LayoutParams roundAt = new LinearLayout.LayoutParams(dp(56), dp(56));
        roundAt.setMarginStart(dp(4));
        bar.addView(round, roundAt);
        held = new ImageView(screen);
        held.setId(R.id.rec_held);
        GradientDrawable redCircle = new GradientDrawable();
        redCircle.setShape(GradientDrawable.OVAL);
        redCircle.setColor(red);
        held.setBackground(redCircle);
        held.setImageDrawable(new Glyph(Glyph.Kind.MIC, screen.getColor(R.color.on_send)));
        held.setPadding(dp(14), dp(14), dp(14), dp(14));
        round.addView(held, new FrameLayout.LayoutParams(dp(56), dp(56)));
        send = new ImageButton(screen);
        send.setId(R.id.rec_send);
        send.setBackground(screen.getDrawable(R.drawable.send_circle));
        send.setImageDrawable(new Glyph(Glyph.Kind.SEND, screen.getColor(R.color.on_send)));
        send.setPadding(dp(15), dp(15), dp(15), dp(15));
        send.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("rec_send");
            }
        });
        round.addView(send, new FrameLayout.LayoutParams(dp(56), dp(56)));
        verbs.put("rec_send", send);

        // Under the bar: the line when nothing is heard or the cap is
        // reached, and its button to the permission.
        noticeRow = new LinearLayout(screen);
        noticeRow.setOrientation(LinearLayout.HORIZONTAL);
        noticeRow.setGravity(Gravity.CENTER_VERTICAL);
        noticeRow.setPadding(dp(20), 0, dp(12), dp(6));
        noticeRow.setVisibility(View.GONE);
        notice = new TextView(screen);
        notice.setId(R.id.rec_notice);
        notice.setTextSize(14);
        notice.setTextColor(text);
        noticeRow.addView(notice, new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        allow = new Button(screen, null, android.R.attr.borderlessButtonStyle);
        allow.setId(R.id.rec_allow);
        allow.setAllCaps(false);
        allow.setTextColor(screen.getColor(R.color.accent));
        allow.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb("rec_allow");
            }
        });
        noticeRow.addView(allow);

        int at = frame.indexOfChild(compose);
        frame.addView(chipArea, at, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, dp(52) + chipRise + dp(4)));
        frame.addView(bar, at + 1, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        frame.addView(noticeRow, at + 2, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));

        frame.hold(microphone, new RecordFrame.Gesture() {
            @Override
            public boolean down(MotionEvent event) {
                return press(event);
            }

            @Override
            public void event(MotionEvent event) {
                gesture(event);
            }
        });
    }

    private ImageButton button(int id, Glyph.Kind kind, int color) {
        ImageButton b = new ImageButton(screen);
        b.setId(id);
        b.setImageDrawable(new Glyph(kind, color));
        b.setBackground(null);
        b.setPadding(dp(10), dp(10), dp(10), dp(10));
        b.setLayoutParams(new LinearLayout.LayoutParams(dp(44), dp(44)));
        final String verb = screen.getResources().getResourceEntryName(id);
        b.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                verb(verb);
            }
        });
        return b;
    }

    private int dp(float dp) {
        return Math.round(dp * density);
    }

    /** Whether the bar is up. */
    boolean shown() {
        return bar.getVisibility() == View.VISIBLE;
    }

    /** The finger landed on the microphone. */
    private boolean press(MotionEvent event) {
        if (!host.mayRecord()) {
            return false;
        }
        host.stopPlaying();
        fromX = event.getRawX();
        fromY = event.getRawY();
        saidX = 0;
        saidY = 0;
        input("press", event.getEventTime(), 0, 0, 0);
        return true;
    }

    /** The rest of the gesture: the slide in dp, the release, the system's cancel, the chip. */
    private void gesture(MotionEvent event) {
        switch (event.getActionMasked()) {
            case MotionEvent.ACTION_MOVE: {
                int dx = Math.round((event.getRawX() - fromX) / density);
                int dy = Math.round((event.getRawY() - fromY) / density);
                if (dx != saidX || dy != saidY) {
                    saidX = dx;
                    saidY = dy;
                    input("slide", 0, dx, dy, 0);
                }
                break;
            }
            case MotionEvent.ACTION_POINTER_DOWN: {
                int i = event.getActionIndex();
                int[] at = new int[2];
                frame.getLocationOnScreen(at);
                if (RecordFrame.on(chip, at[0] + event.getX(i), at[1] + event.getY(i))) {
                    verb("rec_lock");
                }
                break;
            }
            case MotionEvent.ACTION_UP:
                input("release", event.getEventTime(), 0, 0, 0);
                break;
            case MotionEvent.ACTION_CANCEL:
                input("cancel", 0, 0, 0, 0);
                break;
            default:
                break;
        }
    }

    /** One input to the library, then its answer. */
    private void input(final String input, final long now, final int dx, final int dy,
            final int rms) {
        final String peer = host.peer();
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.record(peer, input, now, dx, dy, rms);
            }
        }, said);
    }

    /** One button of the bar, then the library's answer. */
    void verb(final String verb) {
        final String peer = host.peer();
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.recordingVerb(peer, verb);
            }
        }, said);
    }

    /** What an input or a button handed to the engine said (1446), then the library's answer. */
    private final Engine.Said said = new Engine.Said() {
        @Override
        public void said(String error) {
            if (error != null) {
                host.line(error);
            }
            answer();
        }
    };

    /** The screen goes: a recording in any state goes nowhere. */
    void leave() {
        if (recorder != null || shown()) {
            verb("rec_discard");
        }
        player.stop();
    }

    /** The chat's answer says a recording is up that no recorder serves: let it go. */
    void adopt(String recording) {
        if (!recording.isEmpty() && recorder == null) {
            verb("rec_discard");
        }
    }

    void destroy() {
        player.stop();
        preview.shutdownNow();
    }

    /**
     * The library's answer, asked on the engine's thread (1464) right
     * behind the command it follows, so its effects come in their order.
     */
    private void answer() {
        final String peer = host.peer();
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.recording(peer);
            }
        }, new Engine.Got<Map<String, Object>>() {
            @Override
            public void got(Map<String, Object> answer) {
                if (!screen.isDestroyed()) {
                    answered(answer);
                }
            }
        });
    }

    private void answered(Map<String, Object> answer) {
        if (!Core.bool(answer, "ok")) {
            host.line(Core.str(answer, "error"));
            return;
        }
        for (String effect : Core.words(answer, "effects")) {
            run(effect, answer);
        }
        draw(answer);
    }

    /** One of the library's effects. */
    private void run(String effect, Map<String, Object> answer) {
        switch (effect) {
            case "haptic_start":
                frame.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY);
                break;
            case "haptic_lock":
            case "haptic_cap":
                frame.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS);
                break;
            case "start": {
                final Voice.Recorder started = new Voice.Recorder(screen,
                        (int) Core.num(answer, "window_ms"), new Voice.Recorder.Feed() {
                            @Override
                            public void level(final long now, final int rms) {
                                ui.post(new Runnable() {
                                    @Override
                                    public void run() {
                                        if (recorder != null) input("level", now, 0, 0, rms);
                                    }
                                });
                            }
                        });
                if (!started.start()) {
                    host.line(screen.getString(R.string.voice_no_microphone));
                    ui.post(new Runnable() {
                        @Override
                        public void run() {
                            verb("rec_discard");
                        }
                    });
                    break;
                }
                recorder = started;
                break;
            }
            case "pause":
            case "stop":
                if (recorder != null) recorder.pause();
                break;
            case "resume":
                if (recorder != null) recorder.resume();
                break;
            case "finish": {
                player.stop();
                Voice.Recorder done = recorder;
                recorder = null;
                if (done != null) host.send(done.stop(), done.rate());
                break;
            }
            case "discard": {
                player.stop();
                Voice.Recorder done = recorder;
                recorder = null;
                if (done != null) done.stop();
                break;
            }
            case "play": {
                if (recorder == null) break;
                final byte[] pcm = recorder.pcm();
                final int rate = recorder.rate();
                preview.execute(new Runnable() {
                    @Override
                    public void run() {
                        player.play(0, pcm, rate, new Voice.Listener() {
                            @Override
                            public void progress(long key, float done) {}

                            @Override
                            public void ended(long key) {
                                input("played", 0, 0, 0, 0);
                            }
                        });
                    }
                });
                break;
            }
            case "stop_play":
                player.stop();
                break;
            case "ask_microphone":
                // After this answer is drawn: the way there discards the
                // recording, which is an answer of its own.
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        host.askMicrophone();
                    }
                });
                break;
            default:
                break;
        }
    }

    /** The bar as the library has it, or the compose row when there is none. */
    private void draw(Map<String, Object> answer) {
        boolean up = Core.bool(answer, "shown");
        String now = Core.str(answer, "phase");
        moving = up && Core.bool(answer, "moving");
        ui.removeCallbacks(tick);
        if (moving) ui.postDelayed(tick, TICK_MS);
        log(answer, now);
        if (!up) {
            chipArea.setVisibility(View.GONE);
            bar.setVisibility(View.GONE);
            noticeRow.setVisibility(View.GONE);
            compose.setVisibility(View.VISIBLE);
            return;
        }
        compose.setVisibility(View.GONE);
        bar.setVisibility(View.VISIBLE);
        int red = screen.getColor(R.color.rec_red);
        int accent = screen.getColor(R.color.accent);
        dot.set(Core.bool(answer, "pulsing"), red);
        String words = Core.str(answer, "timer");
        if (!words.contentEquals(timer.getText())) timer.setText(words);
        timer.setTextColor(screen.getColor(Core.bool(answer, "amber") ? R.color.rec_amber
                : R.color.text));
        bar.setContentDescription(words);
        Object levels = answer.get("wave");
        wave.set(levels instanceof byte[] ? (byte[]) levels : null, moving,
                (int) Core.num(answer, "wave_slots"), accent);
        wave.setContentDescription(Core.str(answer, "wave_label"));
        cancelDp = (int) Math.max(1, Core.num(answer, "cancel_dp"));
        String hint = Core.str(answer, "cancel");
        cancel.setVisibility(hint.isEmpty() ? View.GONE : View.VISIBLE);
        if (!hint.isEmpty()) {
            // The hint follows the finger as far as the cancel distance.
            String shown = "‹ " + hint;
            if (!shown.contentEquals(cancel.getText())) cancel.setText(shown);
            boolean armed = Core.bool(answer, "armed");
            cancel.setTextColor(armed ? red : screen.getColor(R.color.text_secondary));
            cancel.setTranslationX(-Core.num(answer, "cancel_progress") / 100f
                    * Math.min(cancelDp, 48) * density);
        }
        String heldWords = Core.str(answer, "held");
        held.setVisibility(heldWords.isEmpty() ? View.GONE : View.VISIBLE);
        held.setContentDescription(heldWords);
        held.setScaleX(heldWords.isEmpty() ? 1f : 1.15f);
        held.setScaleY(heldWords.isEmpty() ? 1f : 1.15f);
        String lock = Core.str(answer, "lock");
        chipArea.setVisibility(lock.isEmpty() ? View.GONE : View.VISIBLE);
        if (!lock.isEmpty()) {
            boolean closed = Core.bool(answer, "lock_closed");
            chip.setImageDrawable(new Glyph(closed ? Glyph.Kind.LOCK : Glyph.Kind.UNLOCK,
                    closed ? accent : screen.getColor(R.color.text)));
            chip.setContentDescription(lock);
            chip.setSelected(closed);
            chip.setEnabled(!closed);
            chip.setTranslationY(-Core.num(answer, "lock_progress") / 100f * chipRise);
        }
        buttons.removeAllViews();
        send.setVisibility(View.GONE);
        for (Map<String, Object> button : Core.list(answer, "buttons")) {
            String id = Core.str(button, "id");
            View view = verbs.get(id);
            if (view == null) continue;
            view.setContentDescription(Core.str(button, "label"));
            if (view == send) {
                send.setVisibility(View.VISIBLE);
            } else {
                buttons.addView(view);
            }
        }
        String said = Core.str(answer, "notice");
        noticeRow.setVisibility(said.isEmpty() ? View.GONE : View.VISIBLE);
        if (!said.contentEquals(notice.getText())) notice.setText(said);
        String allowWords = Core.str(answer, "notice_allow");
        allow.setVisibility(allowWords.isEmpty() ? View.GONE : View.VISIBLE);
        allow.setText(allowWords);
    }

    /**
     * The device walk's line, in a debuggable build, whenever what it
     * checks changes: the phase, the dot, the line, the chip, the
     * buttons. Not every window: the timer and the wave are on screen.
     */
    private void log(Map<String, Object> answer, String now) {
        if (!debuggable) return;
        StringBuilder ids = new StringBuilder();
        List<Map<String, Object>> list = Core.list(answer, "buttons");
        for (Map<String, Object> button : list) {
            if (ids.length() > 0) ids.append(',');
            ids.append(Core.str(button, "id"));
        }
        String line = "REC_BAR phase=" + now
                + " pulsing=" + Core.bool(answer, "pulsing")
                + " locked=" + Core.bool(answer, "lock_closed")
                + " armed=" + Core.bool(answer, "armed")
                + " buttons=" + ids
                + " notice=" + Core.str(answer, "notice");
        if (!line.equals(logged)) {
            logged = line;
            Log.i("loquax", line);
        }
    }
}
