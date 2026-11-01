package network.leviculum.loquax;

import android.content.Context;
import android.graphics.Rect;
import android.util.AttributeSet;
import android.view.MotionEvent;
import android.view.View;
import android.widget.LinearLayout;

/**
 * The compose area (1276): the compose row, and the recording bar that
 * takes its place while a voice message records. A finger that lands on
 * the microphone is this frame's for the whole gesture, so the views under
 * it may come and go (the field gives way to the bar the moment it lands)
 * while the slide and the release still arrive. A second finger on the
 * lock chip while the first holds is the chip's tap.
 */
public class RecordFrame extends LinearLayout {
    /** What the gesture does: the recording bar's. */
    interface Gesture {
        /** The finger landed on the microphone; false to leave it alone. */
        boolean down(MotionEvent event);

        /** A later event of the same gesture. */
        void event(MotionEvent event);
    }

    private Gesture gesture;
    private View microphone;
    private boolean tracking;

    public RecordFrame(Context context) {
        super(context);
    }

    public RecordFrame(Context context, AttributeSet attrs) {
        super(context, attrs);
    }

    /** The microphone a gesture starts on, and what it does. */
    void hold(View microphone, Gesture gesture) {
        this.microphone = microphone;
        this.gesture = gesture;
    }

    /** Whether the screen point (`x`, `y`) is on `view` as drawn. */
    static boolean on(View view, float x, float y) {
        if (view == null || !view.isShown()) {
            return false;
        }
        Rect shown = new Rect();
        if (!view.getGlobalVisibleRect(shown)) {
            return false;
        }
        return shown.contains((int) x, (int) y);
    }

    @Override
    public boolean onInterceptTouchEvent(MotionEvent event) {
        if (tracking) {
            return true;
        }
        if (event.getActionMasked() == MotionEvent.ACTION_DOWN && gesture != null
                && on(microphone, event.getRawX(), event.getRawY())) {
            if (gesture.down(event)) {
                tracking = true;
                getParent().requestDisallowInterceptTouchEvent(true);
                return true;
            }
        }
        return false;
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        if (!tracking) {
            return super.onTouchEvent(event);
        }
        if (event.getActionMasked() != MotionEvent.ACTION_DOWN) {
            gesture.event(event);
        }
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            tracking = false;
        }
        return true;
    }
}
