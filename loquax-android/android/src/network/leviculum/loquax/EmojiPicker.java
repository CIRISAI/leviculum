package network.leviculum.loquax;

import android.app.Activity;
import android.app.Dialog;
import android.graphics.drawable.ColorDrawable;
import android.graphics.drawable.GradientDrawable;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.EditText;
import android.widget.GridView;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.PopupWindow;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;

/**
 * The full emoji picker (1137), after Signal's: a sheet from the bottom
 * with a search over the English and German names, the recents as its
 * first row, the categories as tabs, and the emoji of the chosen one; a
 * long press on an emoji with skin tones offers them. loquax's own, as the
 * platform's ready picker is AndroidX; the emoji are the font's, the list
 * is the table of `Emoji`. The reaction row's "…" and the compose field's
 * emoji button open the same picker.
 */
final class EmojiPicker {
    /** What became of the picker. */
    interface Listener {
        void picked(String emoji);
    }

    /** Each category's tab: one of its emoji, in the table's order of categories. */
    private static final String[] TAB_ICONS = {
        "😀", "👋", "🐻", "🍔", "🚗", "⚽", "💡", "🔣", "🏁",
    };
    /** The categories' names, in the table's order (loquax::emoji::CATEGORIES). */
    private static final int[] TAB_NAMES = {
        R.string.emoji_smileys, R.string.emoji_people, R.string.emoji_animals,
        R.string.emoji_food, R.string.emoji_travel, R.string.emoji_activities,
        R.string.emoji_objects, R.string.emoji_symbols, R.string.emoji_flags,
    };
    private static final int COLUMNS = 8;

    private final Activity activity;
    private final Listener listener;
    private final Dialog dialog;
    private final List<Emoji.Category> table;
    private final List<Emoji.Entry> shown = new ArrayList<>();
    private final List<TextView> tabs = new ArrayList<>();
    private final Cells cells = new Cells();
    private int category;
    /** Whether the names said are the German ones. */
    private final boolean german = "de".equals(java.util.Locale.getDefault().getLanguage());

    private EmojiPicker(Activity activity, Listener listener) {
        this.activity = activity;
        this.listener = listener;
        this.table = Emoji.table(activity);
        dialog = new Dialog(activity);
        dialog.requestWindowFeature(Window.FEATURE_NO_TITLE);
        dialog.setContentView(content());
        select(0);
    }

    /** Open the picker; `listener` hears the emoji chosen, and it closes. */
    static void show(Activity activity, Listener listener) {
        EmojiPicker picker = new EmojiPicker(activity, listener);
        picker.dialog.show();
        // The window's size holds once it is shown, not before.
        Window window = picker.dialog.getWindow();
        if (window != null) {
            window.setBackgroundDrawable(new ColorDrawable(0));
            window.setGravity(Gravity.BOTTOM);
            window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT,
                    Math.round(activity.getResources().getDisplayMetrics().heightPixels * 0.55f));
            window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        }
    }

    private int dp(float dp) {
        return Math.round(dp * activity.getResources().getDisplayMetrics().density);
    }

    /** The recents row (1464), into `into` once the library answered. */
    private void recents(LinearLayout into, List<String> recents) {
        if (recents.isEmpty()) {
            return;
        }
        HorizontalScrollView scroll = new HorizontalScrollView(activity);
        scroll.setHorizontalScrollBarEnabled(false);
        LinearLayout row = new LinearLayout(activity);
        for (final String emoji : recents) {
            TextView cell = cell(emoji);
            cell.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    pick(emoji);
                }
            });
            row.addView(cell, new LinearLayout.LayoutParams(dp(44), dp(48)));
        }
        scroll.addView(row);
        TextView label = label(R.string.emoji_recent);
        into.addView(label);
        into.addView(scroll);
    }

    private View content() {
        LinearLayout sheet = new LinearLayout(activity);
        sheet.setOrientation(LinearLayout.VERTICAL);
        GradientDrawable ground = new GradientDrawable();
        float r = dp(20);
        ground.setCornerRadii(new float[] {r, r, r, r, 0, 0, 0, 0});
        ground.setColor(activity.getColor(R.color.background));
        sheet.setBackground(ground);
        sheet.setPadding(dp(8), dp(12), dp(8), 0);

        final EditText search = new EditText(activity);
        search.setHint(R.string.emoji_search);
        search.setSingleLine(true);
        search.setTextColor(activity.getColor(R.color.text));
        search.setHintTextColor(activity.getColor(R.color.text_secondary));
        search.setBackgroundResource(R.drawable.compose_pill);
        search.setPadding(dp(16), dp(10), dp(16), dp(10));
        search.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {}

            @Override
            public void afterTextChanged(Editable s) {
                filter(s.toString());
            }
        });
        sheet.addView(search, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        // The recents, asked on the engine's thread (1464), go here when they come.
        final LinearLayout recentsBox = new LinearLayout(activity);
        recentsBox.setOrientation(LinearLayout.VERTICAL);
        sheet.addView(recentsBox, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        Engine.ask(new Engine.Ask<List<String>>() {
            @Override
            public List<String> ask() {
                return Core.recents();
            }
        }, new Engine.Got<List<String>>() {
            @Override
            public void got(List<String> recents) {
                if (!activity.isDestroyed()) {
                    recents(recentsBox, recents);
                }
            }
        });

        HorizontalScrollView tabScroll = new HorizontalScrollView(activity);
        tabScroll.setHorizontalScrollBarEnabled(false);
        LinearLayout tabRow = new LinearLayout(activity);
        for (int i = 0; i < table.size(); i++) {
            final int index = i;
            TextView tab = cell(i < TAB_ICONS.length ? TAB_ICONS[i] : table.get(i).key);
            tab.setTextSize(20);
            if (i < TAB_NAMES.length) {
                tab.setContentDescription(activity.getString(TAB_NAMES[i]));
            }
            tab.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    search.setText("");
                    select(index);
                }
            });
            tabs.add(tab);
            tabRow.addView(tab, new LinearLayout.LayoutParams(dp(44), dp(44)));
        }
        tabScroll.addView(tabRow);
        sheet.addView(tabScroll, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        GridView grid = new GridView(activity);
        grid.setNumColumns(COLUMNS);
        grid.setStretchMode(GridView.STRETCH_COLUMN_WIDTH);
        grid.setAdapter(cells);
        grid.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                pick(shown.get(position).text);
            }
        });
        grid.setOnItemLongClickListener(new AdapterView.OnItemLongClickListener() {
            @Override
            public boolean onItemLongClick(AdapterView<?> parent, View view, int position, long id) {
                Emoji.Entry entry = shown.get(position);
                if (entry.variants.length == 0) {
                    return false;
                }
                tones(view, entry);
                return true;
            }
        });
        sheet.addView(grid, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        return sheet;
    }

    private TextView label(int text) {
        TextView label = new TextView(activity);
        label.setText(text);
        label.setTextSize(13);
        label.setTextColor(activity.getColor(R.color.text_secondary));
        label.setPadding(dp(8), dp(8), dp(8), 0);
        return label;
    }

    private TextView cell(String emoji) {
        TextView cell = new TextView(activity);
        cell.setText(emoji);
        cell.setTextSize(26);
        cell.setGravity(Gravity.CENTER);
        cell.setBackgroundResource(android.R.drawable.list_selector_background);
        return cell;
    }

    private void select(int index) {
        category = index;
        for (int i = 0; i < tabs.size(); i++) {
            tabs.get(i).setAlpha(i == index ? 1f : 0.45f);
        }
        shown.clear();
        if (index < table.size()) {
            shown.addAll(table.get(index).entries);
        }
        cells.notifyDataSetChanged();
    }

    private void filter(String query) {
        if (query.trim().isEmpty()) {
            select(category);
            return;
        }
        for (TextView tab : tabs) {
            tab.setAlpha(0.45f);
        }
        shown.clear();
        shown.addAll(Emoji.search(activity, query));
        cells.notifyDataSetChanged();
    }

    /** The skin tones of `entry` in a row over its cell: the plain one first. */
    private void tones(View anchor, Emoji.Entry entry) {
        LinearLayout row = new LinearLayout(activity);
        GradientDrawable ground = new GradientDrawable();
        ground.setCornerRadius(dp(24));
        ground.setColor(activity.getColor(R.color.bar));
        row.setBackground(ground);
        row.setPadding(dp(4), dp(4), dp(4), dp(4));
        final PopupWindow popup = new PopupWindow(row,
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, true);
        List<String> all = new ArrayList<>();
        all.add(entry.text);
        for (String variant : entry.variants) {
            all.add(variant);
        }
        for (final String emoji : all) {
            TextView cell = cell(emoji);
            cell.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    popup.dismiss();
                    pick(emoji);
                }
            });
            row.addView(cell, new LinearLayout.LayoutParams(dp(44), dp(48)));
        }
        popup.setElevation(dp(6));
        popup.showAsDropDown(anchor, 0, -anchor.getHeight() - dp(60));
    }

    private void pick(String emoji) {
        dialog.dismiss();
        listener.picked(emoji);
    }

    private final class Cells extends BaseAdapter {
        @Override
        public int getCount() {
            return shown.size();
        }

        @Override
        public Object getItem(int position) {
            return shown.get(position);
        }

        @Override
        public long getItemId(int position) {
            return position;
        }

        @Override
        public View getView(int position, View view, ViewGroup parent) {
            TextView cell = view instanceof TextView ? (TextView) view : cell("");
            Emoji.Entry entry = shown.get(position);
            cell.setText(entry.text);
            cell.setContentDescription(german ? entry.de : entry.en);
            cell.setMinHeight(dp(48));
            return cell;
        }
    }
}
