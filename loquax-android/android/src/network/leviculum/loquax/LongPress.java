package network.leviculum.loquax;

import android.app.Activity;
import android.app.Dialog;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.ColorDrawable;
import android.graphics.drawable.GradientDrawable;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;

/**
 * A long press on a bubble (1137), after Signal's: the chat dims, the
 * pressed bubble stays bright where it was, a row of quick reactions with
 * "…" for the full picker floats above it and a list of actions below; the
 * two swap sides where the screen's edge leaves no room, and a bubble too
 * tall for both keeps them at the screen's top and bottom. A tap outside,
 * or Back, closes it.
 */
final class LongPress {
    /** What the user chose. */
    interface Listener {
        /** One of the quick reactions, or one from the full picker. */
        void react(String emoji);

        /** One of the actions, by its string id. */
        void action(int action);
    }

    private static final int GAP = 8;

    private final Activity activity;
    private final Dialog dialog;
    private final Listener listener;
    /** The actions' words where their string id is not enough; may be null. */
    private CharSequence[] labels;

    private LongPress(Activity activity, Listener listener) {
        this.activity = activity;
        this.listener = listener;
        dialog = new Dialog(activity, android.R.style.Theme_Translucent_NoTitleBar);
        dialog.requestWindowFeature(Window.FEATURE_NO_TITLE);
    }

    /**
     * Over `bubble`, ours or theirs, the reactions `quick` with `current`
     * (ours on it, or null) marked when `reactions`, and `actions` (string
     * ids) under it.
     */
    static void show(Activity activity, View bubble, boolean ours, boolean reactions,
            String[] quick, String current, int[] actions, Listener listener) {
        show(activity, bubble, ours, reactions, quick, current, actions, null, listener);
    }

    /**
     * The same, with `labels` worded for the actions whose string needs a
     * number (1184: "Show all 14 positions"); null or a null entry words
     * the action by its string id.
     */
    static void show(Activity activity, View bubble, boolean ours, boolean reactions,
            String[] quick, String current, int[] actions, CharSequence[] labels,
            Listener listener) {
        LongPress press = new LongPress(activity, listener);
        press.labels = labels;
        press.open(bubble, ours, reactions, quick, current, actions);
    }

    private int dp(float dp) {
        return Math.round(dp * activity.getResources().getDisplayMetrics().density);
    }

    private void open(final View bubble, final boolean ours, boolean reactions, String[] quick,
            String current, int[] actions) {
        final FrameLayout root = new FrameLayout(activity);
        root.setBackgroundColor(activity.getColor(R.color.dim));
        root.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                dialog.dismiss();
            }
        });
        final ImageView copy = new ImageView(activity);
        copy.setImageBitmap(snapshot(bubble));
        copy.setContentDescription(null);
        root.addView(copy, new FrameLayout.LayoutParams(bubble.getWidth(), bubble.getHeight()));
        final View row = reactions ? reactionRow(quick, current) : null;
        if (row != null) {
            root.addView(row, wrap());
        }
        final View list = actionList(actions);
        root.addView(list, wrap());
        root.addOnLayoutChangeListener(new View.OnLayoutChangeListener() {
            @Override
            public void onLayoutChange(View v, int l, int t, int r, int b,
                    int ol, int ot, int or, int ob) {
                place(root, bubble, ours, copy, row, list);
            }
        });
        dialog.setContentView(root);
        Window window = dialog.getWindow();
        if (window != null) {
            window.setBackgroundDrawable(new ColorDrawable(0));
            window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT);
            window.addFlags(WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS);
        }
        dialog.show();
    }

    private static FrameLayout.LayoutParams wrap() {
        return new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    private static Bitmap snapshot(View view) {
        Bitmap bitmap = Bitmap.createBitmap(Math.max(view.getWidth(), 1),
                Math.max(view.getHeight(), 1), Bitmap.Config.ARGB_8888);
        view.draw(new Canvas(bitmap));
        return bitmap;
    }

    /** The bubble where it is on the screen, the row and the list beside it. */
    private void place(FrameLayout root, View bubble, boolean ours, View copy, View row, View list) {
        int[] at = new int[2];
        bubble.getLocationOnScreen(at);
        int[] origin = new int[2];
        root.getLocationOnScreen(origin);
        int x = at[0] - origin[0];
        int y = at[1] - origin[1];
        int w = bubble.getWidth();
        int h = bubble.getHeight();
        int height = root.getHeight();
        int width = root.getWidth();
        int gap = dp(GAP);
        int edge = dp(16);
        int rowH = row == null ? 0 : row.getMeasuredHeight();
        int listH = list.getMeasuredHeight();
        int top = y;
        int rowY;
        int listY;
        boolean above = row == null || y - gap - rowH >= edge;
        boolean below = y + h + gap + listH <= height - edge;
        if (above && below) {
            rowY = y - gap - rowH;
            listY = y + h + gap;
        } else if (!above && y + h + gap + rowH + gap + listH <= height - edge) {
            // No room above: both below.
            rowY = y + h + gap;
            listY = rowY + rowH + gap;
        } else if (!below && y - gap - rowH - gap - listH >= edge) {
            // No room below: both above, the list nearer the bubble.
            listY = y - gap - listH;
            rowY = listY - gap - rowH;
        } else {
            // A tall bubble: the row at the top, the list at the bottom.
            rowY = edge;
            listY = height - edge - listH;
            top = Math.max(y, rowY + rowH + gap);
        }
        move(copy, x, top);
        if (row != null) {
            move(row, side(ours, x, w, row.getMeasuredWidth(), width, edge), rowY);
        }
        move(list, side(ours, x, w, list.getMeasuredWidth(), width, edge), listY);
    }

    /** A panel's left on the bubble's side, inside the screen. */
    private static int side(boolean ours, int x, int w, int panel, int width, int edge) {
        int left = ours ? x + w - panel : x;
        return Math.max(edge, Math.min(left, width - edge - panel));
    }

    private static void move(View view, int x, int y) {
        FrameLayout.LayoutParams params = (FrameLayout.LayoutParams) view.getLayoutParams();
        if (params.leftMargin == x && params.topMargin == y) {
            return;
        }
        params.leftMargin = x;
        params.topMargin = y;
        params.gravity = Gravity.TOP | Gravity.START;
        view.setLayoutParams(params);
    }

    private GradientDrawable panel(float radius) {
        GradientDrawable ground = new GradientDrawable();
        ground.setCornerRadius(radius);
        ground.setColor(activity.getColor(R.color.panel));
        return ground;
    }

    private View reactionRow(String[] quick, String current) {
        LinearLayout row = new LinearLayout(activity);
        row.setBackground(panel(dp(28)));
        row.setElevation(dp(6));
        row.setPadding(dp(6), dp(4), dp(6), dp(4));
        row.setGravity(Gravity.CENTER_VERTICAL);
        for (final String emoji : quick) {
            TextView cell = new TextView(activity);
            cell.setText(emoji);
            cell.setTextSize(26);
            cell.setGravity(Gravity.CENTER);
            if (emoji.equals(current)) {
                GradientDrawable mark = new GradientDrawable();
                mark.setShape(GradientDrawable.OVAL);
                mark.setColor(activity.getColor(R.color.reaction_ours));
                cell.setBackground(mark);
            }
            cell.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    dialog.dismiss();
                    listener.react(emoji);
                }
            });
            row.addView(cell, new LinearLayout.LayoutParams(dp(44), dp(44)));
        }
        TextView more = new TextView(activity);
        more.setText("…");
        more.setTextSize(22);
        more.setGravity(Gravity.CENTER);
        more.setTextColor(activity.getColor(R.color.text));
        more.setContentDescription(activity.getString(R.string.emoji_more));
        GradientDrawable circle = new GradientDrawable();
        circle.setShape(GradientDrawable.OVAL);
        circle.setColor(activity.getColor(R.color.bar));
        more.setBackground(circle);
        more.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                dialog.dismiss();
                EmojiPicker.show(activity, new EmojiPicker.Listener() {
                    @Override
                    public void picked(String emoji) {
                        listener.react(emoji);
                    }
                });
            }
        });
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(dp(40), dp(40));
        params.setMarginStart(dp(4));
        row.addView(more, params);
        row.setClickable(true);
        row.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED);
        return row;
    }

    private View actionList(int[] actions) {
        LinearLayout list = new LinearLayout(activity);
        list.setOrientation(LinearLayout.VERTICAL);
        list.setBackground(panel(dp(16)));
        list.setElevation(dp(6));
        list.setPadding(0, dp(6), 0, dp(6));
        // Taps between the items stay on the list, not the dim that closes.
        list.setClickable(true);
        for (int i = 0; i < actions.length; i++) {
            final int action = actions[i];
            TextView item = new TextView(activity);
            if (labels != null && i < labels.length && labels[i] != null) {
                item.setText(labels[i]);
            } else {
                item.setText(action);
            }
            item.setTextSize(16);
            item.setTextColor(activity.getColor(action == R.string.action_delete
                    ? R.color.failed_mark_on_panel : R.color.text));
            item.setMinWidth(dp(200));
            item.setPadding(dp(20), dp(12), dp(24), dp(12));
            item.setBackgroundResource(android.R.drawable.list_selector_background);
            item.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    dialog.dismiss();
                    listener.action(action);
                }
            });
            list.addView(item);
        }
        list.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED);
        return list;
    }
}
