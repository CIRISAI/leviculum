package network.leviculum.loquax;

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

    private Core() {}

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
        Map<String, Object> request = map("dir", dir, "offset", (long) offset(), "lang", lang());
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
        Map<String, Object> request = map("dir", dir, "offset", (long) offset(), "lang", lang(), "here", true);
        if (restore != null) {
            request.put("restore", restore);
            request.put("passphrase", passphrase);
        }
        return answer(Native.start(MsgPack.pack(request)));
    }

    /** The address's backup line and its paper code, sealed with `passphrase`; `error` when not. */
    static Map<String, Object> backup(String passphrase) {
        return answer(Native.query(MsgPack.pack(map("t", "backup", "passphrase", passphrase))));
    }

    /** This phone's address, its QR code, its name and whether the mailbox is here. */
    static Map<String, Object> me() {
        return answer(Native.query(MsgPack.pack(map("t", "me"))));
    }

    /** The profile behind our avatar (1187): name, address, contact code and its QR, mailbox, advanced facts; `error` when not. */
    static Map<String, Object> profile() {
        return answer(Native.query(MsgPack.pack(map("t", "profile"))));
    }

    /** Whose contact code `text` is (`address`, `lxma`, `known`); `error` when it is none. */
    static Map<String, Object> lxma(String text) {
        return answer(Native.query(MsgPack.pack(map("t", "lxma", "text", text))));
    }

    /** The contact of `lxma` under `name` (none when empty), its key to the mailbox, its chat open. */
    static String addContact(String lxma, String name) {
        return command(map("t", "addcontact", "lxma", lxma, "name", name));
    }

    /** The name recipients see, for the mailbox on this phone. Blocks a moment: not on the UI thread. */
    static String name(String name) {
        return command(map("t", "name", "name", name));
    }

    /** One request; the error in words, or null. */
    static String command(Map<String, Object> request) {
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

    /** Delete the messages `keys`, asked already. */
    static String delete(String peer, List<Long> keys) {
        return command(map("t", "delete", "peer", peer, "keys", keys));
    }

    /** An emoji picked into a message: first in the recents. */
    static String used(String emoji) {
        return command(map("t", "used", "emoji", emoji));
    }

    /** A message's detail lines, the terminal's `v` view; null with the error in `error[0]`. */
    @SuppressWarnings("unchecked")
    static List<String> info(String peer, long key, String[] error) {
        Map<String, Object> answer = answer(Native.query(MsgPack.pack(
                map("t", "info", "peer", peer, "key", key))));
        if (!(answer.get("rows") instanceof List)) {
            error[0] = str(answer, "error");
            return null;
        }
        return strings(answer.get("rows"));
    }

    /** The emoji used last, newest first; none without a session. */
    static List<String> recents() {
        return strings(answer(Native.query(MsgPack.pack(map("t", "recents")))).get("emoji"));
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
        return list(answer(Native.query(MsgPack.pack(
                map("t", "suggest", "peer", peer, "candidates", candidates)))), "suggestions");
    }

    /** Every address linked to the contact `lookup`. */
    static List<Map<String, Object>> contact(String lookup) {
        return list(answer(Native.query(MsgPack.pack(map("t", "contact", "lookup", lookup)))),
                "addresses");
    }

    /**
     * The reports the row `key` of `peer`'s conversation folds (1184),
     * newest first: key, at, lat_e6, lon_e6, place, accuracy, fix_at.
     */
    static List<Map<String, Object>> run(String peer, long key) {
        return list(answer(Native.query(MsgPack.pack(
                map("t", "run", "peer", peer, "key", key)))), "positions");
    }

    /**
     * Everyone with a position (1200), newest first: the map's pins and
     * its Friends sheet. Each row peer, name, initials, icon_fg, icon_bg,
     * photo, at, lat_e6, lon_e6, distance_m (only with the phone's fix),
     * recent (the last 24 h), stale, geo; our own apart under "me". An
     * "error" key without a session.
     */
    static Map<String, Object> friends() {
        return answer(Native.query(MsgPack.pack(map("t", "friends"))));
    }

    /** The same, with distances from the phone's own fix (1201), degrees times a million. */
    static Map<String, Object> friends(long latE6, long lonE6) {
        return answer(Native.query(MsgPack.pack(map("t", "friends", "lat_e6", latE6, "lon_e6", lonE6))));
    }

    /**
     * The positions we shared, newest first (1201): each row peer, name,
     * initials, icon_fg, icon_bg, photo, at, lat_e6, lon_e6 under
     * "shared"; an "error" key without a session.
     */
    static Map<String, Object> shared() {
        return answer(Native.query(MsgPack.pack(map("t", "shared"))));
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
        return answer(Native.query(MsgPack.pack(request)));
    }

    /** Every link: peer, lookup, contact_id, name. */
    static List<Map<String, Object>> links() {
        return list(answer(Native.query(MsgPack.pack(map("t", "links")))), "links");
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
        return answer(Native.query(MsgPack.pack(map("t", "detail", "peer", peer))));
    }

    /** The archived conversations; with `search` only those the library's search finds (1217). */
    static Map<String, Object> archived(String search) {
        return answer(Native.query(MsgPack.pack(search == null
                ? map("t", "archived") : map("t", "archived", "search", search))));
    }

    static Map<String, Object> blocked() {
        return answer(Native.query(MsgPack.pack(map("t", "blocked"))));
    }

    static String pull() {
        return command(map("t", "pull"));
    }

    /** A screen came to the front or left it: in front, a down watching link pulls every minute. */
    static String front(boolean front) {
        return command(map("t", "front", "front", front));
    }

    /**
     * The conversation list; with `search` only the rows the library's
     * search finds, every word in the name or the last line (1217).
     */
    static Map<String, Object> conversations(String search) {
        return answer(Native.query(MsgPack.pack(search == null
                ? map("t", "conversations") : map("t", "conversations", "search", search))));
    }

    static Map<String, Object> messages(String peer) {
        return answer(Native.query(MsgPack.pack(map("t", "messages", "peer", peer))));
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
        return answer(Native.query(MsgPack.pack(request)));
    }

    /**
     * The map's fling (1188): `set` stored as the config's `map_fling`, or
     * with null only asked; the choice in force as `fling`. Waits for the
     * frame a map may be drawing: not on the UI thread.
     */
    static Map<String, Object> mapFling(Boolean set) {
        Map<String, Object> request = map("t", "mapfling");
        if (set != null) request.put("set", set);
        return answer(Native.query(MsgPack.pack(request)));
    }

    /** The next event, or null; blocks up to waitMs. */
    static Map<String, Object> nextEvent(int waitMs) {
        byte[] bytes = Native.nextEvent(waitMs);
        return bytes == null ? null : answer(bytes);
    }
}
