package network.leviculum.loquax;

import android.graphics.Canvas;
import android.graphics.ColorFilter;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.PixelFormat;
import android.graphics.Rect;
import android.graphics.drawable.Drawable;

/**
 * The few icons the chat and the list draw, as paths on a 24 by 24 grid
 * scaled to the bounds: no icon font, no AndroidX, no image assets (1129,
 * the list's search, close and pencil 1130, the tab bar's chat and pin
 * 1199, the map's people button 1200, the crosshair and the chat's
 * attachment menu 1201, the microphone and the voice player 1222). The delivery marks are the ladder of
 * docs/src/concepts/loquax-read-receipts.md, section 3 (1232): CLOCK, TICK,
 * TICKS, TICKS_READ, FAILED, with the numbers of
 * loquax/src/ui/chat.rs's shapes, which a test of the facade reads here.
 */
final class Glyph extends Drawable {
    enum Kind {
        BACK, MENU, EMOJI, SEND, CLOCK, TICK, TICKS, TICKS_READ, FAILED, SEARCH, CLOSE, PENCIL, CHAT,
        PIN, PEOPLE, LOCATE, ATTACH, MIC, PLAY, PAUSE, CAMERA, PLUS, LOCK, UNLOCK, BIN, STOP, UP, HASH
    }

    private final Kind kind;
    private final int color;
    /** The failed mark's circle and its exclamation mark. */
    private final int failedGround;
    private final int failedMark;
    private final Paint stroke = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
    private int alpha = 255;

    /** The mark of one of ours, by the facade's delivery word. */
    static Kind mark(String state) {
        switch (state) {
            case "mailbox": return Kind.TICK;
            case "arrived": return Kind.TICK;
            case "delivered": return Kind.TICKS;
            case "read": return Kind.TICKS_READ;
            case "failed": return Kind.FAILED;
            default: return Kind.CLOCK;
        }
    }

    /** The view id a mark is found by in the device walk (chat::Mark::resource). */
    static int resource(Kind kind) {
        switch (kind) {
            case TICK: return R.id.state_tick;
            case TICKS: return R.id.state_ticks;
            case TICKS_READ: return R.id.state_ticks_read;
            case FAILED: return R.id.state_failed;
            default: return R.id.state_clock;
        }
    }

    Glyph(Kind kind, int color) {
        this(kind, color, 0, 0);
    }

    Glyph(Kind kind, int color, int failedGround, int failedMark) {
        this.kind = kind;
        this.color = color;
        this.failedGround = failedGround;
        this.failedMark = failedMark;
        stroke.setStyle(Paint.Style.STROKE);
        stroke.setStrokeCap(Paint.Cap.ROUND);
        stroke.setStrokeJoin(Paint.Join.ROUND);
        fill.setStyle(Paint.Style.FILL);
    }

    @Override
    public void draw(Canvas canvas) {
        Rect b = getBounds();
        float side = Math.min(b.width(), b.height());
        canvas.saveLayerAlpha(b.left, b.top, b.right, b.bottom, alpha);
        canvas.translate(b.centerX() - side / 2, b.centerY() - side / 2);
        canvas.scale(side / 24f, side / 24f);
        stroke.setColor(color);
        fill.setColor(color);
        stroke.setStrokeWidth(2f);
        switch (kind) {
            case BACK:
                canvas.drawLine(4, 12, 20, 12, stroke);
                canvas.drawPath(path(11, 5, 4, 12, 11, 19), stroke);
                break;
            case MENU:
                canvas.drawCircle(12, 5, 2, fill);
                canvas.drawCircle(12, 12, 2, fill);
                canvas.drawCircle(12, 19, 2, fill);
                break;
            case EMOJI:
                canvas.drawCircle(12, 12, 9, stroke);
                canvas.drawCircle(9, 10, 1.3f, fill);
                canvas.drawCircle(15, 10, 1.3f, fill);
                Path smile = new Path();
                smile.moveTo(8, 14);
                smile.quadTo(12, 18, 16, 14);
                canvas.drawPath(smile, stroke);
                break;
            case SEND:
                Path arrow = path(4, 20, 21, 12, 4, 4);
                arrow.lineTo(6.5f, 12);
                arrow.close();
                canvas.drawPath(arrow, fill);
                break;
            case CLOCK:
                stroke.setStrokeWidth(1.8f);
                canvas.drawCircle(12, 12, 9, stroke);
                canvas.drawPath(path(12, 7, 12, 12, 15.5f, 14), stroke);
                break;
            case TICK:
                stroke.setStrokeWidth(2f);
                canvas.drawPath(path(5, 12.5f, 9.5f, 17, 19, 7), stroke);
                break;
            case TICKS:
            case TICKS_READ:
                // Two bare ticks 6.5 units apart, the second's short leg
                // cut clear of the first; read in a heavier stroke.
                stroke.setStrokeWidth(kind == Kind.TICKS_READ ? 2.6f : 2f);
                canvas.drawPath(path(1.5f, 12.5f, 6, 17, 15.5f, 7), stroke);
                canvas.drawPath(path(11.6f, 16.1f, 12.5f, 17, 22, 7), stroke);
                break;
            case FAILED:
                fill.setColor(failedGround);
                canvas.drawCircle(12, 12, 10, fill);
                stroke.setColor(failedMark);
                stroke.setStrokeWidth(2.4f);
                canvas.drawLine(12, 6.5f, 12, 13, stroke);
                fill.setColor(failedMark);
                canvas.drawCircle(12, 17, 1.4f, fill);
                break;
            case SEARCH:
                canvas.drawCircle(10.5f, 10.5f, 6, stroke);
                canvas.drawLine(15, 15, 20, 20, stroke);
                break;
            case CLOSE:
                canvas.drawLine(6, 6, 18, 18, stroke);
                canvas.drawLine(18, 6, 6, 18, stroke);
                break;
            case PENCIL:
                Path pencil = path(4, 20, 5, 15.5f, 15.5f, 5, 19, 8.5f, 8.5f, 19);
                pencil.close();
                canvas.drawPath(pencil, stroke);
                canvas.drawLine(13, 7.5f, 16.5f, 11, stroke);
                break;
            case CHAT:
                // A speech bubble with its tail at the lower left.
                Path bubble = path(5, 5, 19, 5, 19, 16, 10, 16, 5, 20, 5, 5);
                bubble.close();
                canvas.drawPath(bubble, stroke);
                break;
            case PIN:
                // A map pin: a drop with a hole, its point at the bottom.
                Path drop = new Path();
                drop.moveTo(12, 21);
                drop.cubicTo(7, 15.5f, 5, 12.5f, 5, 9.5f);
                drop.cubicTo(5, 5.5f, 8.2f, 3, 12, 3);
                drop.cubicTo(15.8f, 3, 19, 5.5f, 19, 9.5f);
                drop.cubicTo(19, 12.5f, 17, 15.5f, 12, 21);
                drop.close();
                canvas.drawPath(drop, stroke);
                canvas.drawCircle(12, 9.5f, 2.5f, stroke);
                break;
            case PEOPLE:
                // Two heads and shoulders, the one behind to the right.
                canvas.drawCircle(9, 8, 3.2f, stroke);
                Path front = new Path();
                front.moveTo(3, 20);
                front.cubicTo(3, 15.5f, 5.7f, 13.5f, 9, 13.5f);
                front.cubicTo(12.3f, 13.5f, 15, 15.5f, 15, 20);
                canvas.drawPath(front, stroke);
                canvas.drawCircle(16.5f, 7, 2.6f, stroke);
                Path behind = new Path();
                behind.moveTo(16.5f, 12);
                behind.cubicTo(19.3f, 12, 21, 14, 21, 17.5f);
                canvas.drawPath(behind, stroke);
                break;
            case HASH:
                // A room's sign (1384): two slanted uprights, two bars.
                canvas.drawLine(10, 4, 8, 20, stroke);
                canvas.drawLine(16, 4, 14, 20, stroke);
                canvas.drawLine(4.5f, 9, 19.5f, 9, stroke);
                canvas.drawLine(4.5f, 15, 19.5f, 15, stroke);
                break;
            case ATTACH:
                // A paper clip: two nested rounded loops, open at the top.
                Path clip = new Path();
                clip.moveTo(15.5f, 7);
                clip.lineTo(15.5f, 16);
                clip.cubicTo(15.5f, 18.2f, 13.9f, 20, 12, 20);
                clip.cubicTo(10.1f, 20, 8.5f, 18.2f, 8.5f, 16);
                clip.lineTo(8.5f, 6.5f);
                clip.cubicTo(8.5f, 5.1f, 9.6f, 4, 11, 4);
                clip.cubicTo(12.4f, 4, 13.5f, 5.1f, 13.5f, 6.5f);
                clip.lineTo(13.5f, 15.5f);
                clip.cubicTo(13.5f, 16.3f, 12.8f, 17, 12, 17);
                clip.cubicTo(11.2f, 17, 10.5f, 16.3f, 10.5f, 15.5f);
                clip.lineTo(10.5f, 8);
                canvas.drawPath(clip, stroke);
                break;
            case LOCATE:
                // A ring with four ticks and a dot: the crosshair.
                canvas.drawCircle(12, 12, 6.5f, stroke);
                canvas.drawCircle(12, 12, 2.2f, fill);
                canvas.drawLine(12, 2, 12, 5.5f, stroke);
                canvas.drawLine(12, 18.5f, 12, 22, stroke);
                canvas.drawLine(2, 12, 5.5f, 12, stroke);
                canvas.drawLine(18.5f, 12, 22, 12, stroke);
                break;
            case MIC:
                // A capsule on a stand: the microphone (1222).
                canvas.drawRoundRect(9, 3, 15, 15, 3, 3, fill);
                Path cup = new Path();
                cup.moveTo(6, 11);
                cup.cubicTo(6, 15, 8.7f, 18, 12, 18);
                cup.cubicTo(15.3f, 18, 18, 15, 18, 11);
                canvas.drawPath(cup, stroke);
                canvas.drawLine(12, 18, 12, 21, stroke);
                break;
            case CAMERA:
                // A body with a hump on top and a lens (1270).
                Path body = path(3, 8, 7.5f, 8, 9, 5.5f, 15, 5.5f, 16.5f, 8, 21, 8, 21, 19, 3, 19);
                body.close();
                canvas.drawPath(body, stroke);
                canvas.drawCircle(12, 13, 3.5f, stroke);
                break;
            case PLUS:
                // The compose bar's round button with nothing to send (1270).
                canvas.drawLine(12, 5, 12, 19, stroke);
                canvas.drawLine(5, 12, 19, 12, stroke);
                break;
            case PLAY:
                Path play = path(8, 5, 19, 12, 8, 19);
                play.close();
                canvas.drawPath(play, fill);
                break;
            case PAUSE:
                canvas.drawRect(7, 5, 10.5f, 19, fill);
                canvas.drawRect(13.5f, 5, 17, 19, fill);
                break;
            case LOCK:
            case UNLOCK:
                // The recording bar's lock chip (1276): a body and its
                // shackle, closed, or open with its right leg lifted.
                canvas.drawRoundRect(5.5f, 11, 18.5f, 20.5f, 2, 2, fill);
                Path shackle = new Path();
                shackle.moveTo(8.5f, 11);
                shackle.lineTo(8.5f, 7.5f);
                shackle.cubicTo(8.5f, 5.3f, 10, 3.5f, 12, 3.5f);
                shackle.cubicTo(14, 3.5f, 15.5f, 5.3f, 15.5f, 7.5f);
                shackle.lineTo(15.5f, kind == Kind.LOCK ? 11 : 8.5f);
                canvas.save();
                if (kind == Kind.UNLOCK) canvas.translate(0, -2);
                canvas.drawPath(shackle, stroke);
                canvas.restore();
                break;
            case BIN:
                // The bin: a lid with its handle and a tapering can.
                canvas.drawLine(4.5f, 6.5f, 19.5f, 6.5f, stroke);
                canvas.drawPath(path(9.5f, 6.5f, 9.5f, 4, 14.5f, 4, 14.5f, 6.5f), stroke);
                Path can = path(6.5f, 6.5f, 7.5f, 20, 16.5f, 20, 17.5f, 6.5f);
                canvas.drawPath(can, stroke);
                canvas.drawLine(10.5f, 10, 10.5f, 16.5f, stroke);
                canvas.drawLine(13.5f, 10, 13.5f, 16.5f, stroke);
                break;
            case STOP:
                canvas.drawRoundRect(6.5f, 6.5f, 17.5f, 17.5f, 1.5f, 1.5f, fill);
                break;
            case UP:
                canvas.drawPath(path(6, 15, 12, 9, 18, 15), stroke);
                break;
            default:
                break;
        }
        canvas.restore();
    }

    private static Path path(float... xy) {
        Path p = new Path();
        p.moveTo(xy[0], xy[1]);
        for (int i = 2; i + 1 < xy.length; i += 2) {
            p.lineTo(xy[i], xy[i + 1]);
        }
        return p;
    }

    @Override
    public void setAlpha(int alpha) {
        this.alpha = alpha;
        invalidateSelf();
    }

    @Override
    public void setColorFilter(ColorFilter filter) {
        stroke.setColorFilter(filter);
        fill.setColorFilter(filter);
    }

    /** Abstract in Drawable though deprecated, so it has to be here. */
    @Override
    @SuppressWarnings("deprecation")
    public int getOpacity() {
        return PixelFormat.TRANSLUCENT;
    }
}
