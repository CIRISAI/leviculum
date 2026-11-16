package network.leviculum.loquax;

import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TimeZone;

/**
 * The library's calls as maps, in schema version 1
 * (loquax-android/src/schema.rs). Thin on purpose: what a message, a send
 * or a mark does is decided in Rust.
 */
final class Core {
    static final long VERSION = 1;
    /** The phone has a camera (1270): set by {@link #sense} before a start. */
    private static volatile boolean camera;

    private Core() {}

    /** What the phone can do that the library's compose bar asks: a camera. */
    static void sense(Context context) {
        camera = context.getPackageManager().hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY);
    }

    static Map<String, Object> map(Object... pairs) {
        Map<String, Object> map = new LinkedHashMap<>();
        for (int i = 0; i + 1 < pairs.length; i += 2) {
            map.put((String) pairs[i], pairs[i + 1]);
        }
        return map;
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> answer(byte[] bytes) {
        if (bytes == null || bytes.length == 0) {
            return map("ok", false, "error", "the library gave no answer");
        }
        Object value = MsgPack.unpack(bytes);
        if (!(value instanceof Map)) {
            return map("ok", false, "error", "the library's answer is no map");
        }
        Map<String, Object> answer = (Map<String, Object>) value;
        if (num(answer, "v") != VERSION) {
            return map("ok", false, "error",
                    "the library speaks schema " + num(answer, "v") + ", this app " + VERSION);
        }
        return answer;
    }

    static String str(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value == null ? "" : String.valueOf(value);
    }

    static long num(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value instanceof Number ? ((Number) value).longValue() : 0;
    }

    /** A real of an answer: a float, or an integer as one. */
    static double real(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value instanceof Number ? ((Number) value).doubleValue() : 0;
    }

    /** A list of strings of an answer, empty for none (1276: the recorder's effects). */
    static List<String> words(Map<String, Object> map, String key) {
        return strings(map.get(key));
    }

    /** A map inside an answer, empty for none (1443: the profile's words). */
    @SuppressWarnings("unchecked")
    static Map<String, Object> sub(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value instanceof Map ? (Map<String, Object>) value : new java.util.HashMap<String, Object>();
    }

    static boolean bool(Map<String, Object> map, String key) {
        return Boolean.TRUE.equals(map.get(key));
    }

    @SuppressWarnings("unchecked")
    static List<Map<String, Object>> list(Map<String, Object> map, String key) {
        Object value = map.get(key);
        List<Map<String, Object>> out = new ArrayList<>();
        if (value instanceof List) {
            for (Object item : (List<Object>) value) {
                if (item instanceof Map) {
                    out.add((Map<String, Object>) item);
                }
            }
        }
        return out;
    }

    /** Seconds east of UTC, now. */
    static int offset() {
        return TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 1000;
    }

    /** The phone's language as a locale tag (de-DE): the library says its words in it (1217). */
    static String lang() {
        return Locale.getDefault().toLanguageTag();
    }

    /** Start; code and server null on a later start. Blocks: not on the UI thread. */
    static Map<String, Object> start(String dir, String server, String code, String name) {
        Engine.offMain("Native.start");
        Map<String, Object> request = map("dir", dir, "offset", (long) offset(), "lang", lang(),
                "camera", camera);
        if (server != null) request.put("server", server);
        if (code != null) request.put("code", code);
        if (name != null) request.put("name", name);
        return answer(Native.start(MsgPack.pack(request)));
    }

    /**
     * Start with the mailbox on this phone (1152): the library makes it, and
     * joins the screens to it itself; with a backup line and its passphrase
     * it restores the address first. Blocks: not on the UI thread.
     */
    static Map<String, Object> startHere(String dir, String restore, String passphrase) {
        return startHere(dir, restore, passphrase, false);
    }

    /**
     * {@link #startHere}, and with `anyway` a restore goes on though its address answers on another
     * device right now (1281): the user saw the warning (`need` live) and chose to.
     */
    static Map<String, Object> startHere(String dir, String restore, String passphrase, boolean anyway) {
        Map<String, Object> request = map("dir", dir, "offset", (long) offset(), "lang", lang(), "here", true,
                "camera", camera);
        if (restore != null) {
            request.put("restore", restore);
            request.put("passphrase", passphrase);
        }
        if (anyway) request.put("anyway", true);
        return answer(Native.start(MsgPack.pack(request)));
    }

    /**
     * Move in from the old phone (1281): the library takes the mailbox over with the code the old
     * phone's "Move to a new phone" shows, then starts it here. `name` is how the old phone names this
     * one. Blocks: not on the UI thread.
     */
    static Map<String, Object> moveIn(String dir, String code, String name) {
        return answer(Native.start(MsgPack.pack(map("dir", dir, "offset", (long) offset(), "lang", lang(),
                "here", true, "camera", camera, "moving", code, "name", name))));
    }

    /** "Use this phone again" on a phone whose mailbox moved away (1281). Blocks: not on the UI thread. */
    static Map<String, Object> takeBack(String dir) {
        return answer(Native.start(MsgPack.pack(map("dir", dir, "offset", (long) offset(), "lang", lang(),
                "camera", camera, "take_back", true))));
    }

    /** The address's backup line and its paper code, sealed with `passphrase`; `error` when not. */
    static Map<String, Object> backup(String passphrase) {
        return query(map("t", "backup", "passphrase", passphrase));
    }

    /**
     * Whether `text` is a code and which kind, never asking the session
     * (1464): `known` is empty. The UI thread may ask; {@link #lxma} adds
     * the name the session knows for it.
     */
    static Map<String, Object> code(String text) {
        return look(map("t", "code", "text", text, "lang", lang()));
    }

    /** This phone's address, its QR code, its name and whether the mailbox is here. */
    static Map<String, Object> me() {
        return query(map("t", "me"));
    }

    /** The profile behind our avatar (1187): name, address, contact code and its QR, mailbox, advanced facts; `error` when not. */
    static Map<String, Object> profile() {
        return query(map("t", "profile"));
    }

    /** The settings' mailbox page (1642): `title`, `groups` of `title` and `lines` (`label`, `value`, `copy`, `action`, `note`), `copy_label`, `copied`; `error` when not. */
    static Map<String, Object> mailboxPage() {
        return query(map("t", "mailboxpage"));
    }

    /** A fresh join code of the mailbox on this phone for another device (1264): `code`, `qr`, `qr_size`, `hours`; `error` when not. */
    static Map<String, Object> linkDevice() {
        return query(map("t", "linkdevice"));
    }

    /** Whether a device came with the newest link code: `state` waiting, linked (with `name`), over or none. */
    static Map<String, Object> linked() {
        return query(map("t", "linked"));
    }

    /** A fresh code for the new phone (1281), as {@link #linkDevice}'s; `error` when not. */
    static Map<String, Object> movePhone() {
        return query(map("t", "movephone"));
    }

    /** Where the newest move code stands: `state` waiting, moved (with `name`), over or none. */
    static Map<String, Object> moving() {
        return query(map("t", "moving"));
    }

    /** Whose contact code `text` is (`address`, `lxma`, `known`); `error` when it is none. */
    static Map<String, Object> lxma(String text) {
        return query(map("t", "lxma", "text", text, "lang", lang()));
    }

    /** The contact of `lxma` under `name` (none when empty), its key to the mailbox, its chat open. */
    static String addContact(String lxma, String name) {
        return command(map("t", "addcontact", "lxma", lxma, "name", name));
    }

    /**
     * The profile came to the front or left it (1443): a tap on the avatar is the core's
     * `Intent::Profile` here as in the window and the terminal, and the profile's words are the
     * core's scene. The error in words, or null.
     */
    static String profileShown(boolean open) {
        return command(map("t", "profile", "open", open));
    }

    /** The name recipients see, for the mailbox on this phone. Blocks a moment: not on the UI thread. */
    static String name(String name) {
        return command(map("t", "name", "name", name));
    }

    /**
     * A verb on groups (1307): `new`, `pick`, `next`, `name`, `create`,
     * `join`, `members`, `leave`, `remove`, `close`, with the group (or the
     * friend picked) in `peer`; the error in words, or null.
     */
    static String group(String verb, String peer, String member, String text, boolean confirmed) {
        return command(map("t", "group", "verb", verb, "peer", peer, "member", member,
                "text", text, "confirmed", confirmed));
    }

    /** "New group" or the open member sheet (1307), as the library words it. */
    static Map<String, Object> groupScreen() {
        return query(map("t", "group"));
    }

    /**
     * A verb of the Browse tab (1337): `tab`, `chats`, `back`, `forward`,
     * `reload`, `places`, `dismiss`, `activate` with a scene node, `go` with
     * the address, `fill` with a field and its text, `scroll` with `up`,
     * `down`, `pageup` or `pagedown`; the error in words, or null.
     */
    static String browse(String verb, String node, String text) {
        return command(map("t", "browse", "verb", verb, "node", node, "text", text));
    }

    /** The page view's size in cells, which the page is laid out on (1337). */
    static String browseSize(int cols, int rows) {
        return command(map("t", "browse", "verb", "size", "cols", cols, "rows", rows));
    }

    /** The Browse tab as the library words and lays it out (1337). */
    static Map<String, Object> browseScreen() {
        return query(map("t", "browse"));
    }

    /**
     * A verb of the Rooms tab (1384): `tab`, `chats`, `show` with the
     * list's index of a room (-1 for the list), `say` with the input
     * field's text, which the library reads as the terminal's input line;
     * the error in words, or null.
     */
    static String rooms(String verb, String text, int index) {
        return command(map("t", "rooms", "verb", verb, "text", text, "index", index));
    }

    /** The Rooms tab as the library words it (1384). */
    static Map<String, Object> roomsScreen() {
        return query(map("t", "rooms"));
    }

    /**
     * A verb of the Explore tab (1578): `tab`, `chats`, `typed` with the
     * search field's text, `az` (index 1 for A to Z), `open` and
     * `bookmark` with a page's destination, `address`, `room` with the
     * index of a room the reader is in, `hub` with a hub's destination;
     * the error in words, or null.
     */
    static String explore(String verb, String text, String dest, int index) {
        return command(map("t", "explore", "verb", verb, "text", text, "dest", dest, "index", index));
    }

    /** The Explore tab as the library words it (1578). */
    static Map<String, Object> exploreScreen() {
        return query(map("t", "explore"));
    }

    /** The settings' sections (1450), the one list of every target: `sections` with `key`, `title`, `summary`. */
    static Map<String, Object> settingsList() {
        return query(map("t", "settings"));
    }

    /**
     * One question; its answer. Waits on the session's lock as a command
     * does: never on the UI thread, where {@link Engine#offMain} guards it;
     * screens go through {@link Engine#ask} (1464).
     */
    static Map<String, Object> query(Map<String, Object> request) {
        Engine.offMain("Native.query " + request.get("t"));
        return answer(Native.query(MsgPack.pack(request)));
    }

    /**
     * One of the questions the library answers without waiting for the
     * session's lock (1464): the two lists, from the scene its pump left.
     * The UI thread may ask these.
     */
    static Map<String, Object> look(Map<String, Object> request) {
        return answer(Native.query(MsgPack.pack(request)));
    }

    /**
     * One request; the error in words, or null. Waits on the session's
     * lock: never on the UI thread, where {@link Engine#offMain} guards it;
     * screens go through {@link Engine#run} (1446).
     */
    static String command(Map<String, Object> request) {
        Engine.offMain("Native.command " + request.get("t"));
        Map<String, Object> done = answer(Native.command(MsgPack.pack(request)));
        return bool(done, "ok") ? null : str(done, "error");
    }

    static String open(String peer) {
        return command(map("t", "open", "peer", peer));
    }

    static String close() {
        return command(map("t", "close"));
    }

    static String send(String peer, String body) {
        return command(map("t", "send", "peer", peer, "body", body));
    }

    /**
     * A reply typed into the notification of `peer`'s messages (1506):
     * `key` is the row's `notice_key` the notification was posted with.
     * The library reads every message up to it, and its read receipt rides
     * on the reply.
     */
    static String noticeReply(String peer, long key, String body) {
        return command(map("t", "noticereply", "peer", peer, "key", key, "body", body));
    }

    static String react(String peer, String target, String content) {
        return command(map("t", "react", "peer", peer, "target", target, "content", content));
    }

    /**
     * What the compose field of `peer`'s chat holds now (1219), and the id of
     * the message a reply being written answers, "" for none: the library
     * keeps it as the conversation's draft.
     */
    static String draft(String peer, String body, String reply) {
        return command(map("t", "draft", "peer", peer, "body", body, "reply", reply));
    }

    /** A reply to the message `target` (1137): sent with its id and a quote of it. */
    static String reply(String peer, String target, String body) {
        return command(map("t", "reply", "peer", peer, "target", target, "body", body));
    }

    /** A copy of the message `key` of `peer`'s conversation to `to`. */
    static String forward(String peer, long key, String to) {
        return command(map("t", "forward", "peer", peer, "key", key, "to", to));
    }

    /** Send the message `key`, one of ours not sent, again: its red mark's tap (1232). */
    static String sendAgain(String peer, long key) {
        return command(map("t", "sendagain", "peer", peer, "key", key));
    }

    /**
     * A picture the user picked with the paperclip's "Photo" (1220), as the
     * picker gave it: the library scales and strips it and sends it.
     */
    static String photo(String peer, byte[] data) {
        return command(map("t", "photo", "peer", peer, "data", data));
    }

    /**
     * The photo of the message `key`, upright and scaled into an `edge`
     * square by the library (1220), as a Bitmap; null when it has none or
     * it does not decode. Not on the UI thread.
     */
    /**
     * One gesture on the photo viewer (1269), folded into the state
     * `state` holds (its `view_w`, `view_h`, `picture_w`, `picture_h`,
     * `scale`, `pan_x`, `pan_y`) by the library's zoom, which needs no
     * session: the state after it, where the picture lands and what a
     * coast has room for (schema.rs, ZoomShown), itself such a state. `gesture` is the
     * gesture's word and its reals: `factor`, `x`, `y`, `dx`, `dy`.
     */
    static Map<String, Object> zoom(Map<String, Object> state, String gesture, Object... reals) {
        Map<String, Object> request = map("t", "zoom",
                "view_w", real(state, "view_w"), "view_h", real(state, "view_h"),
                "picture_w", real(state, "picture_w"), "picture_h", real(state, "picture_h"),
                "scale", real(state, "scale"),
                "pan_x", real(state, "pan_x"), "pan_y", real(state, "pan_y"),
                "gesture", gesture);
        for (int i = 0; i + 1 < reals.length; i += 2) {
            request.put((String) reals[i], ((Number) reals[i + 1]).doubleValue());
        }
        // No session is asked (lib.rs, Query::Zoom): the UI thread's own.
        return answer(Native.query(MsgPack.pack(request)));
    }

    /**
     * The photo the camera took for `peer` (1270), as the capture wrote
     * it: the library scales and strips it as a picked one and stages it
     * in the compose bar. Not on the UI thread.
     */
    static String stage(String peer, byte[] data) {
        return command(map("t", "stage", "peer", peer, "data", data));
    }

    /** The staged photo's cross. */
    static String unstage(String peer) {
        return command(map("t", "unstage", "peer", peer));
    }

    /** The compose bar's sheet: `attach`, `position`, or "" to close it. */
    static String sheet(String peer, String kind) {
        return command(map("t", "sheet", "peer", peer, "kind", kind));
    }

    /** The phone knows no own fix: the position sheet says so. */
    static String noFix() {
        return command(map("t", "nofix"));
    }

    /** The position sheet's "Send my position now": the last fix to `peer`. */
    static String shareFix(String peer) {
        return command(map("t", "sharefix", "peer", peer));
    }

    /**
     * A share of the position for `peer`'s chat (1273): `span` is
     * `15_min`, `1_h`, `8_h` or `until_stopped`.
     */
    static String startShare(String peer, String span) {
        return command(map("t", "startshare", "peer", peer, "span", span));
    }

    /** The share's Stop: one last report marked as the end (1273). */
    static String stopShare(String peer) {
        return command(map("t", "stopshare", "peer", peer));
    }

    /** The running shares: Settings, Privacy and the share notification (1273). */
    static Map<String, Object> shares() {
        return query(map("t", "shares"));
    }

    /** `peer`'s compose bar alone: its buttons, its staged photo, its sheet (1270). */
    static Map<String, Object> bar(String peer) {
        return query(map("t", "bar", "peer", peer));
    }

    /** The staged photo's pixels in an `edge` box, or null. Not on the UI thread. */
    static Bitmap stagedBitmap(String peer, int edge) {
        return pixels(query(
                map("t", "staged", "peer", peer, "edge", (long) edge)), null);
    }

    static Bitmap photoBitmap(String peer, long key, int edge) {
        return photoBitmap(peer, key, edge, null);
    }

    /**
     * The photo's pixels, scaled into `edge` by `edge`; with `full` not
     * null, its two ints set to the picture's own size before the scale
     * (1269, what the viewer zooms to).
     */
    static Bitmap photoBitmap(String peer, long key, int edge, int[] full) {
        return pixels(query(
                map("t", "photo", "peer", peer, "key", key, "edge", (long) edge)), full);
    }

    /** A PhotoPixels answer as a Bitmap, `full` as in photoBitmap; null for none. */
    private static Bitmap pixels(Map<String, Object> answer, int[] full) {
        Object rgba = answer.get("rgba");
        long w = num(answer, "width");
        long h = num(answer, "height");
        if (!(rgba instanceof byte[]) || w <= 0 || h <= 0
                || ((byte[]) rgba).length != w * h * 4) {
            return null;
        }
        if (full != null) {
            // An older library sends no full size: the pixels' own.
            long fw = num(answer, "full_width");
            long fh = num(answer, "full_height");
            full[0] = (int) (fw > 0 ? fw : w);
            full[1] = (int) (fh > 0 ? fh : h);
        }
        Bitmap bitmap = Bitmap.createBitmap((int) w, (int) h, Bitmap.Config.ARGB_8888);
        bitmap.copyPixelsFromBuffer(java.nio.ByteBuffer.wrap((byte[]) rgba));
        return bitmap;
    }

    /**
     * The Chats page's photo size (1220): `set` (small, phone, original)
     * stored, or with null only asked; the size in force as "size".
     */
    static Map<String, Object> photoSize(String set) {
        Map<String, Object> request = map("t", "photosize");
        if (set != null) request.put("set", set);
        return query(request);
    }

    /**
     * A file the user picked with the paperclip's "File" (1221): its name
     * and the bytes read, `size` the whole file's bytes counted where the
     * reading stopped early, else 0. The library sends it, or says why not.
     */
    static String file(String peer, String name, byte[] data, long size) {
        return command(map("t", "file", "peer", peer, "name", name, "data", data, "size", size));
    }

    /**
     * The `index`th file of the message `key`, saved by the library under
     * the app's files (1221): "path" and "name"; "error" when it cannot
     * be. Not on the UI thread.
     */
    static Map<String, Object> saveFile(String peer, long key, int index) {
        return query(
                map("t", "savefile", "peer", peer, "key", key, "index", (long) index));
    }

    /**
     * A recording the microphone made (1222): mono 16-bit PCM, little
     * endian, at `rate`. The library codes it as Opus in Ogg and sends it,
     * or says why not (too short, too long).
     */
    static String voice(String peer, byte[] pcm, int rate) {
        return command(map("t", "voice", "peer", peer, "pcm", pcm, "rate", (long) rate));
    }

    /**
     * The play button of the message `key` (`on`), its pause button or the
     * player's end (1222): the library's bubble follows.
     */
    static String playVoice(String peer, long key, boolean on) {
        return command(map("t", "playvoice", "peer", peer, "key", key, "on", on));
    }

    /**
     * The voice message of the message `key`, decoded by the library
     * (1222): "rate" and "pcm" (mono 16-bit little endian); "error" when it
     * does not play. Not on the UI thread.
     */
    static Map<String, Object> voicePcm(String peer, long key) {
        return query(map("t", "voice", "peer", peer, "key", key));
    }

    /**
     * The microphone's gesture or the recorder's level for `peer`'s
     * recording (1276): `press`, `slide` (`dx`, `dy` dp from where the
     * finger landed), `release`, `cancel`, `level` (`rms` of the window),
     * `tick` or `played`, at `now` on the uptime clock. Every field goes,
     * each a long; {@link #recording} has what to do next.
     */
    static String record(String peer, String input, long now, int dx, int dy, int rms) {
        return command(map("t", "record", "peer", peer, "input", input, "now", now,
                "dx", (long) dx, "dy", (long) dy, "rms", (long) rms));
    }

    /** A button of the recording bar, by its id (`rec_lock` … `rec_allow`, 1276). */
    static String recordingVerb(String peer, String verb) {
        return command(map("t", "recording", "peer", peer, "verb", verb));
    }

    /**
     * `peer`'s recording bar and the recorder's work since the last ask
     * (1276): `phase`, `shown`, `pulsing`, `timer`, `wave` (bytes), the
     * hint, the chip, `buttons`, `notice`, `effects`, and the library's
     * distances `cancel_dp` and `lock_dp`.
     */
    static Map<String, Object> recording(String peer) {
        return query(map("t", "recording", "peer", peer));
    }

    /** Delete the messages `keys`, asked already. */
    static String delete(String peer, List<Long> keys) {
        return command(map("t", "delete", "peer", peer, "keys", keys));
    }

    /** An emoji picked into a message: first in the recents. */
    static String used(String emoji) {
        return command(map("t", "used", "emoji", emoji));
    }

    /**
     * A message's Info (1381): `rows`, each a `label` and a `value` in the phone's language, and
     * `advanced`, the terminal's `v` lines; null with the error in `error[0]`.
     */
    static Map<String, Object> info(String peer, long key, String[] error) {
        Map<String, Object> answer = query(
                map("t", "info", "peer", peer, "key", key));
        if (!(answer.get("rows") instanceof List)) {
            error[0] = str(answer, "error");
            return null;
        }
        return answer;
    }

    /** The emoji used last, newest first; none without a session. */
    static List<String> recents() {
        return strings(query(map("t", "recents")).get("emoji"));
    }

    @SuppressWarnings("unchecked")
    private static List<String> strings(Object value) {
        List<String> out = new ArrayList<>();
        if (value instanceof List) {
            for (Object item : (List<Object>) value) {
                out.add(String.valueOf(item));
            }
        }
        return out;
    }

    /**
     * Link `peer` to the contact `copy` describes (Book.copy): its lookup
     * key, `_ID`, name and thumbnail; again to refresh the copy (1139).
     */
    static String link(String peer, Map<String, Object> copy) {
        Map<String, Object> request = map("t", "link", "peer", peer);
        request.putAll(copy);
        return command(request);
    }

    static String unlink(String peer) {
        return command(map("t", "unlink", "peer", peer));
    }

    /** "Not now" on the banner: it does not ask again for `peer`. */
    static String notNow(String peer) {
        return command(map("t", "notnow", "peer", peer));
    }

    /** The user's own name for `peer`, empty to clear it; every device of the home takes it. */
    static String rename(String peer, String name) {
        return command(map("t", "rename", "peer", peer, "name", name));
    }

    /** The address book's IM rows of protocol LXMF (Book.imRows): what loquax knows is linked. */
    static String relink(List<Map<String, Object>> rows) {
        return command(map("t", "relink", "rows", rows));
    }

    /** Which of `candidates` (Book.candidates) `peer`'s announced name may be, best first. */
    static List<Map<String, Object>> suggest(String peer, List<Map<String, Object>> candidates) {
        return list(query(
                map("t", "suggest", "peer", peer, "candidates", candidates)), "suggestions");
    }

    /** Every address linked to the contact `lookup`. */
    static List<Map<String, Object>> contact(String lookup) {
        return list(query(map("t", "contact", "lookup", lookup)),
                "addresses");
    }

    /**
     * The reports the row `key` of `peer`'s conversation folds (1184),
     * newest first: key, at, lat_e6, lon_e6, place, accuracy, fix_at.
     */
    static List<Map<String, Object>> run(String peer, long key) {
        return list(query(
                map("t", "run", "peer", peer, "key", key)), "positions");
    }

    /**
     * Everyone with a position (1200), newest first: the map's pins and
     * its Friends sheet. Each row peer, name, initials, icon_fg, icon_bg,
     * photo, at, lat_e6, lon_e6, distance_m (only with the phone's fix),
     * recent (the last 24 h), stale, geo; our own apart under "me". An
     * "error" key without a session.
     */
    static Map<String, Object> friends() {
        return query(map("t", "friends"));
    }

    /** The same, with distances from the phone's own fix (1201), degrees times a million. */
    static Map<String, Object> friends(long latE6, long lonE6) {
        return query(map("t", "friends", "lat_e6", latE6, "lon_e6", lonE6));
    }

    /**
     * The positions we shared, newest first (1201): each row peer, name,
     * initials, icon_fg, icon_bg, photo, at, lat_e6, lon_e6 under
     * "shared"; an "error" key without a session.
     */
    static Map<String, Object> shared() {
        return query(map("t", "shared"));
    }

    /**
     * The Maps page (1201, 1207), off the UI thread: with `delete` that
     * region deleted, with `autoload` the automatic download switched; the
     * regions (name, bytes, modified, lat_e6, lon_e6, min_zoom, max_zoom,
     * auto) under "regions", their sum as "region_bytes", "autoload". A
     * question of its own, not a map frame: it carries no frame's fields.
     */
    static Map<String, Object> mapRegions(String delete, Boolean autoload) {
        Map<String, Object> request = map("t", "mapregions");
        if (delete != null) request.put("delete", delete);
        if (autoload != null) request.put("set_autoload", autoload);
        return query(request);
    }

    /**
     * The Maps page's last row, after its question (1290): every
     * downloaded region deleted; null, or the line why not. Off the UI
     * thread, as {@link #mapRegions} is.
     */
    static String deleteRegions() {
        return command(map("t", "deleteregions"));
    }

    /** Every link: peer, lookup, contact_id, name. */
    static List<Map<String, Object>> links() {
        return list(query(map("t", "links")), "links");
    }

    /** Pin `peer`'s conversation to the top, or loosen it (1146); every device of the home. */
    static String pin(String peer, boolean on) {
        return command(map("t", "pin", "peer", peer, "on", on));
    }

    /** Mute for `secs` from now, always when negative, unmute with 0. */
    static String mute(String peer, long secs) {
        return command(map("t", "mute", "peer", peer, "secs", secs));
    }

    static String archive(String peer, boolean on) {
        return command(map("t", "archive", "peer", peer, "on", on));
    }

    static String block(String peer, boolean on) {
        return command(map("t", "block", "peer", peer, "on", on));
    }

    /** The identity compared in person; on this phone only. */
    static String verify(String peer, boolean on) {
        return command(map("t", "verify", "peer", peer, "on", on));
    }

    /** Unread on this phone, or read as opening it would be. */
    static String unread(String peer, boolean on) {
        return command(map("t", "unread", "peer", peer, "on", on));
    }

    /** Delete the conversation here, or on every device of the home. */
    static String forget(String peer, boolean everywhere) {
        return command(map("t", "forget", "peer", peer, "everywhere", everywhere));
    }

    /** The contact screen of `peer`; an "error" key when there is none. */
    static Map<String, Object> detail(String peer) {
        return query(map("t", "detail", "peer", peer));
    }

    /**
     * The archived conversations, as the list shows them: never waits for
     * the session's lock (1464, session.rs `Scene`), so the UI thread may ask.
     */
    static Map<String, Object> archived() {
        return look(map("t", "archived"));
    }

    /** The archived conversations the library's search finds in `search` (1217). */
    static Map<String, Object> findArchived(String search) {
        return query(map("t", "archived", "search", search));
    }

    /**
     * The people a new conversation lists (1448): every person with a conversation, by name; with
     * `search` (null for none) only those the library's search finds.
     */
    static Map<String, Object> people(String search) {
        return search == null ? query(map("t", "people")) : query(map("t", "people", "search", search));
    }

    static Map<String, Object> blocked() {
        return query(map("t", "blocked"));
    }

    static String pull() {
        return command(map("t", "pull"));
    }

    /** A screen came to the front or left it: in front, a down watching link pulls every minute. */
    static String front(boolean front) {
        return command(map("t", "front", "front", front));
    }

    /**
     * These bubbles of `peer`'s chat are on screen, at least half of each, by
     * their keys (1231, 1496): the library decides from them, and from the
     * front, what was read, and loads the page above the first loaded one.
     */
    static String viewport(String peer, List<Long> keys) {
        return command(map("t", "viewport", "peer", peer, "keys", keys));
    }

    /**
     * The phone's network is `kind` now (`none`, `wifi`, `mobile`, `other`),
     * `changed` when it is another network than before (1298, {@link NetWatch}).
     */
    static String network(String kind, boolean changed) {
        return command(map("t", "network", "kind", kind, "changed", changed));
    }

    /**
     * The conversation list: never waits for the session's lock (1464):
     * the library answers from the scene its pump left while the pump
     * holds the lock (session.rs `Scene`), so the UI thread may ask.
     */
    static Map<String, Object> conversations() {
        return look(map("t", "conversations"));
    }

    /**
     * The rows of the list the library's search finds in `search`, every
     * word in the name or the last line (1217). Waits on the lock.
     */
    static Map<String, Object> findConversations(String search) {
        return query(map("t", "conversations", "search", search));
    }

    static Map<String, Object> messages(String peer) {
        return query(map("t", "messages", "peer", peer));
    }

    /**
     * One frame of the map (MapView) drawn into `into`, a bitmap of the
     * frame's size, or a prefetch with `into` null: what it shows. Not on
     * the UI thread.
     */
    static Map<String, Object> renderMap(Map<String, Object> request, Bitmap into) {
        byte[] bytes;
        try {
            bytes = Native.renderMapInto(MsgPack.pack(request), into);
        } catch (RuntimeException e) {
            return map("ok", false, "error", String.valueOf(e.getMessage()));
        }
        if (bytes == null) {
            return map("ok", false, "error", "the library gave no frame");
        }
        return answer(bytes);
    }

    /**
     * The map's theme (1199, 1207): `set` (`auto`, `light`, `dark`)
     * stored, or with null only asked; the choice in force as `theme`.
     * Waits for the frame a map may be drawing: not on the UI thread.
     */
    static Map<String, Object> mapTheme(String set) {
        Map<String, Object> request = map("t", "maptheme");
        if (set != null) request.put("set", set);
        return query(request);
    }

    /**
     * The map's fling (1188): `set` stored as the config's `map_fling`, or
     * with null only asked; the choice in force as `fling`. Waits for the
     * frame a map may be drawing: not on the UI thread.
     */
    static Map<String, Object> mapFling(Boolean set) {
        Map<String, Object> request = map("t", "mapfling");
        if (set != null) request.put("set", set);
        return query(request);
    }

    /**
     * The Privacy page's "Send read receipts" (1233): with `set` stored,
     * the switch in force as "on" either way.
     */
    static Map<String, Object> receipts(Boolean set) {
        Map<String, Object> request = map("t", "receipts");
        if (set != null) request.put("set", set);
        return query(request);
    }

    /**
     * The Advanced page's public rooms' hub (1577): with `set` ("none", or
     * a hub's address) chosen, the lines in force either way.
     */
    static Map<String, Object> roomsHub(String set) {
        Map<String, Object> request = map("t", "roomshub");
        if (set != null) request.put("set", set);
        return query(request);
    }

    /** The next event, or null; blocks up to waitMs. */
    static Map<String, Object> nextEvent(int waitMs) {
        byte[] bytes = Native.nextEvent(waitMs);
        return bytes == null ? null : answer(bytes);
    }
}
