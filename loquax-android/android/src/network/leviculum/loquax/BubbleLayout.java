package network.leviculum.loquax;

import android.content.Context;
import android.text.Layout;
import android.util.AttributeSet;
import android.view.View;
import android.view.ViewGroup;
import android.widget.TextView;

/**
 * A bubble: one shape whatever the message carries (1268). From the top
 * a reply's quote (1137) across the bubble's width, the media (a photo
 * of 1220, file rows of 1221, a voice message of 1222), the body under
 * them, and in the bottom right corner the time and the delivery mark
 * (1129). The time and mark sit on the body's last line when it leaves
 * room for them, as in a modern messenger, and on a row of their own
 * below otherwise; without a body (the scene has no words) they close
 * the media. The bubble is at most {@link #setMaxWidth} wide; a photo
 * wider than that is scaled down to it, its shape kept.
 */
public class BubbleLayout extends ViewGroup {
    /** The media, top first. */
    private static final int[] MEDIA = {R.id.photo, R.id.files, R.id.voice};

    private int maxWidth = Integer.MAX_VALUE;
    private int gap;
    /** Whether the time and mark take a row of their own. */
    private boolean below;
    /** The height of the body's row, the meta's when it shares it. */
    private int bodyRow;

    public BubbleLayout(Context context, AttributeSet attrs) {
        super(context, attrs);
        gap = Math.round(8 * context.getResources().getDisplayMetrics().density);
    }

    void setMaxWidth(int px) {
        if (px != maxWidth) {
            maxWidth = px;
            requestLayout();
        }
    }

    /** The child `id`, when it shows. */
    private View shown(int id) {
        View view = findViewById(id);
        return view != null && view.getVisibility() != GONE ? view : null;
    }

    @Override
    protected void onMeasure(int widthSpec, int heightSpec) {
        int pads = getPaddingLeft() + getPaddingRight();
        int size = MeasureSpec.getMode(widthSpec) == MeasureSpec.UNSPECIFIED
                ? maxWidth : Math.min(MeasureSpec.getSize(widthSpec), maxWidth);
        int room = Math.max(0, size - pads);
        int unspecified = MeasureSpec.makeMeasureSpec(0, MeasureSpec.UNSPECIFIED);
        int atMost = MeasureSpec.makeMeasureSpec(room, MeasureSpec.AT_MOST);
        int w = 0;
        int h = 0;
        View quote = shown(R.id.quote);
        if (quote != null) {
            quote.measure(atMost, unspecified);
            w = quote.getMeasuredWidth();
        }
        for (int id : MEDIA) {
            View media = shown(id);
            if (media == null) {
                continue;
            }
            LayoutParams params = media.getLayoutParams();
            if (params.width > 0 && params.height > 0) {
                // A photo: its size from ChatActivity, scaled into the room.
                int mw = Math.min(params.width, room);
                int mh = (int) ((long) params.height * mw / params.width);
                media.measure(MeasureSpec.makeMeasureSpec(mw, MeasureSpec.EXACTLY),
                        MeasureSpec.makeMeasureSpec(mh, MeasureSpec.EXACTLY));
            } else {
                media.measure(atMost, unspecified);
            }
            w = Math.max(w, media.getMeasuredWidth());
            h += media.getMeasuredHeight() + gap / 2;
        }
        TextView body = (TextView) shown(R.id.body);
        View meta = shown(R.id.meta);
        int bodyW = 0;
        bodyRow = 0;
        if (body != null) {
            body.measure(atMost, unspecified);
            bodyW = body.getMeasuredWidth();
            bodyRow = body.getMeasuredHeight();
        }
        below = true;
        if (meta != null) {
            meta.measure(unspecified, unspecified);
            int mw = meta.getMeasuredWidth();
            int mh = meta.getMeasuredHeight();
            Layout layout = body == null ? null : body.getLayout();
            int last = layout == null ? 0 : (int) Math.ceil(
                    layout.getLineWidth(layout.getLineCount() - 1))
                    + body.getPaddingLeft() + body.getPaddingRight();
            if (body != null && last + gap + mw <= room) {
                below = false;
                bodyW = Math.max(bodyW, last + gap + mw);
                bodyRow = Math.max(bodyRow, mh);
            } else {
                w = Math.max(w, mw);
                h += mh;
            }
        }
        w = Math.max(w, bodyW);
        h += bodyRow;
        if (body == null && h > 0) {
            // The media's last gap, with no body under it.
            h -= meta == null ? gap / 2 : 0;
        }
        if (quote != null) {
            quote.measure(MeasureSpec.makeMeasureSpec(w, MeasureSpec.EXACTLY), unspecified);
            h += quote.getMeasuredHeight() + gap / 2;
        }
        setMeasuredDimension(w + pads, h + getPaddingTop() + getPaddingBottom());
    }

    @Override
    protected void onLayout(boolean changed, int l, int t, int r, int b) {
        int left = getPaddingLeft();
        int y = getPaddingTop();
        View quote = shown(R.id.quote);
        if (quote != null) {
            quote.layout(left, y, left + quote.getMeasuredWidth(), y + quote.getMeasuredHeight());
            y += quote.getMeasuredHeight() + gap / 2;
        }
        for (int id : MEDIA) {
            View media = shown(id);
            if (media != null) {
                media.layout(left, y, left + media.getMeasuredWidth(),
                        y + media.getMeasuredHeight());
                y += media.getMeasuredHeight() + gap / 2;
            }
        }
        View body = shown(R.id.body);
        if (body != null) {
            // Sharing its row with a taller meta, the body sits at its foot.
            int top = y + bodyRow - body.getMeasuredHeight();
            body.layout(left, top, left + body.getMeasuredWidth(), top + body.getMeasuredHeight());
        }
        View meta = shown(R.id.meta);
        if (meta != null) {
            int right = getWidth() - getPaddingRight();
            int bottom = getHeight() - getPaddingBottom();
            meta.layout(right - meta.getMeasuredWidth(), bottom - meta.getMeasuredHeight(),
                    right, bottom);
        }
    }
}
