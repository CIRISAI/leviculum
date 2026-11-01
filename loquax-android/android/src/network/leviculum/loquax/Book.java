package network.leviculum.loquax;

import android.Manifest;
import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.ContentObserver;
import android.database.Cursor;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.provider.ContactsContract;
import android.provider.ContactsContract.CommonDataKinds.Im;
import android.provider.ContactsContract.CommonDataKinds.Nickname;
import android.provider.ContactsContract.CommonDataKinds.StructuredName;
import android.provider.ContactsContract.Contacts;
import android.provider.ContactsContract.Data;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The phone's address book (1139; docs/drafts/loquax-android-contacts-2026-10.md),
 * through platform APIs only. loquax's own table in the library is the
 * truth; what this class reads is copied there once and refreshed while
 * READ_CONTACTS is granted. loquax writes nothing into a contact itself:
 * the address goes into one only through the user's own contacts editor,
 * as an IM row of protocol LXMF, when the user turns the switch on.
 *
 * Without READ_CONTACTS a contact the user picks is read once, right after
 * the pick, by the picker's one-off grant. With it, the suggestions, the
 * refresh of names and photos, and the links from IM rows on a new phone.
 */
final class Book {
    /** The IM row's protocol label, Lew's choice of 2026-10-29. */
    static final String LXMF = "LXMF";
    static final int PICK = 41;
    static final int INSERT = 42;
    static final int EDIT = 43;
    static final int ASK = 44;
    /** A thumbnail larger than this is not copied. */
    private static final int PHOTO_MAX = 256 * 1024;
    /** How long the observer waits for the address book to settle. */
    private static final long SETTLE_MS = 2000;

    private static ContentObserver observer;

    private Book() {}

    static boolean canRead(Context context) {
        return context.checkSelfPermission(Manifest.permission.READ_CONTACTS)
                == PackageManager.PERMISSION_GRANTED;
    }

    /**
     * The phone owner's name from the address book's own profile card ("Me"),
     * for the first start's name field (walk finding 9); empty when there is
     * none or it cannot be read. Android lets an app read the profile only
     * with READ_CONTACTS, which the first start does not ask for: without
     * it the field starts empty.
     */
    static String ownerName(Context context) {
        if (!canRead(context)) {
            return "";
        }
        try (Cursor cursor = context.getContentResolver().query(
                ContactsContract.Profile.CONTENT_URI,
                new String[] {Contacts.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                return name == null ? "" : name.trim();
            }
        } catch (RuntimeException unreadable) {
            // No profile provider, or no grant after all: no name.
        }
        return "";
    }

    /** Ask for READ_CONTACTS, only ever because the user asked for suggestions. */
    static void ask(Activity screen) {
        screen.requestPermissions(new String[] {Manifest.permission.READ_CONTACTS}, ASK);
    }

    private static boolean start(Activity screen, Intent intent, int code) {
        try {
            screen.startActivityForResult(intent, code);
            return true;
        } catch (ActivityNotFoundException | SecurityException none) {
            return false;
        }
    }

    /** The system's contact picker; its answer comes to onActivityResult as PICK. */
    static boolean pick(Activity screen) {
        return start(screen, new Intent(Intent.ACTION_PICK).setType(Contacts.CONTENT_TYPE), PICK);
    }

    /** The IM row that says `hex` is an LXMF address. */
    private static ArrayList<ContentValues> imRow(String hex) {
        ContentValues row = new ContentValues();
        row.put(Data.MIMETYPE, Im.CONTENT_ITEM_TYPE);
        row.put(Im.DATA, hex);
        row.put(Im.TYPE, Im.TYPE_OTHER);
        row.put(Im.PROTOCOL, Im.PROTOCOL_CUSTOM);
        row.put(Im.CUSTOM_PROTOCOL, LXMF);
        ArrayList<ContentValues> rows = new ArrayList<>();
        rows.add(row);
        return rows;
    }

    /**
     * A new contact in the user's editor, named `name`, with the address in
     * it when `withAddress`; the editor shows the account it goes into.
     */
    static boolean insert(Activity screen, String name, String hex, boolean withAddress) {
        Intent intent = new Intent(ContactsContract.Intents.Insert.ACTION)
                .setType(Contacts.CONTENT_TYPE)
                .putExtra("finishActivityOnSaveCompleted", true);
        if (name != null && !name.isEmpty()) {
            intent.putExtra(ContactsContract.Intents.Insert.NAME, name);
        }
        if (withAddress) {
            intent.putParcelableArrayListExtra(ContactsContract.Intents.Insert.DATA, imRow(hex));
        }
        return start(screen, intent, INSERT);
    }

    /** The user's editor on a linked contact, the address's IM row filled in. */
    static boolean saveAddress(Activity screen, long id, String lookup, String hex) {
        Intent intent = new Intent(Intent.ACTION_EDIT, Contacts.getLookupUri(id, lookup))
                .putExtra("finishActivityOnSaveCompleted", true)
                .putParcelableArrayListExtra(ContactsContract.Intents.Insert.DATA, imRow(hex));
        return start(screen, intent, EDIT);
    }

    /** The contact's own card in the contacts app. */
    static boolean open(Activity screen, long id, String lookup) {
        try {
            screen.startActivity(new Intent(Intent.ACTION_VIEW, Contacts.getLookupUri(id, lookup)));
            return true;
        } catch (ActivityNotFoundException | SecurityException none) {
            return false;
        }
    }

    /**
     * What the contact at `uri` says right now, as the library's link
     * takes it: lookup, contact_id, name, photo; null when it cannot be
     * read. Right after a pick the picker's grant covers it; else it needs
     * READ_CONTACTS.
     */
    static Map<String, Object> copy(ContentResolver resolver, Uri uri) {
        String[] columns = {Contacts._ID, Contacts.LOOKUP_KEY, Contacts.DISPLAY_NAME_PRIMARY};
        Map<String, Object> copy = null;
        try (Cursor cursor = resolver.query(uri, columns, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                copy = Core.map(
                        "contact_id", cursor.getLong(0),
                        "lookup", cursor.getString(1),
                        "name", cursor.getString(2));
            }
        } catch (RuntimeException unreadable) {
            return null;
        }
        if (copy == null || Core.str(copy, "lookup").isEmpty() || Core.str(copy, "name").isEmpty()) {
            return null;
        }
        byte[] photo = photo(resolver, Contacts.getLookupUri(
                Core.num(copy, "contact_id"), Core.str(copy, "lookup")));
        if (photo == null) {
            // The picker's grant may cover the URI it gave and no other.
            photo = photo(resolver, uri);
        }
        if (photo != null) {
            copy.put("photo", photo);
        }
        return copy;
    }

    /** The contact's thumbnail's bytes, or null. */
    private static byte[] photo(ContentResolver resolver, Uri contact) {
        try (InputStream in = Contacts.openContactPhotoInputStream(resolver, contact, false)) {
            if (in == null) {
                return null;
            }
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] chunk = new byte[8192];
            int n;
            while ((n = in.read(chunk)) > 0) {
                out.write(chunk, 0, n);
                if (out.size() > PHOTO_MAX) {
                    return null;
                }
            }
            return out.size() > 0 ? out.toByteArray() : null;
        } catch (IOException | RuntimeException unreadable) {
            return null;
        }
    }

    /** The contact with lookup key `lookup`, read with READ_CONTACTS; null when gone. */
    static Map<String, Object> copyOf(ContentResolver resolver, long id, String lookup) {
        Uri found;
        try {
            found = Contacts.lookupContact(resolver, Contacts.getLookupUri(id, lookup));
        } catch (RuntimeException unreadable) {
            return null;
        }
        return found == null ? null : copy(resolver, found);
    }

    /** The contact with lookup key `key`, read with READ_CONTACTS; null when gone. */
    static Map<String, Object> copyByKey(ContentResolver resolver, String key) {
        return copy(resolver, Uri.withAppendedPath(Contacts.CONTENT_LOOKUP_URI, Uri.encode(key)));
    }

    /**
     * Every contact's name fields, for the library's ranking (only with
     * READ_CONTACTS): its key is the lookup key, its fields the structured
     * name and the nicknames. Never leaves the process.
     */
    static List<Map<String, Object>> candidates(ContentResolver resolver) {
        Map<String, Map<String, Object>> byKey = new LinkedHashMap<>();
        String[] columns = {
            Data.LOOKUP_KEY, Data.MIMETYPE, Data.DISPLAY_NAME_PRIMARY,
            StructuredName.GIVEN_NAME, StructuredName.MIDDLE_NAME, StructuredName.FAMILY_NAME,
            Nickname.NAME,
        };
        String where = Data.MIMETYPE + " IN (?, ?)";
        String[] args = {StructuredName.CONTENT_ITEM_TYPE, Nickname.CONTENT_ITEM_TYPE};
        try (Cursor cursor = resolver.query(Data.CONTENT_URI, columns, where, args, null)) {
            while (cursor != null && cursor.moveToNext()) {
                String key = cursor.getString(0);
                if (key == null) {
                    continue;
                }
                Map<String, Object> candidate = byKey.get(key);
                if (candidate == null) {
                    candidate = Core.map("key", key, "display", text(cursor, 2),
                            "nicknames", new ArrayList<String>());
                    byKey.put(key, candidate);
                }
                if (StructuredName.CONTENT_ITEM_TYPE.equals(cursor.getString(1))) {
                    candidate.put("given", text(cursor, 3));
                    candidate.put("middle", text(cursor, 4));
                    candidate.put("family", text(cursor, 5));
                } else if (!text(cursor, 6).isEmpty()) {
                    @SuppressWarnings("unchecked")
                    List<String> nicknames = (List<String>) candidate.get("nicknames");
                    nicknames.add(text(cursor, 6));
                }
            }
        } catch (RuntimeException unreadable) {
            return new ArrayList<>();
        }
        return new ArrayList<>(byKey.values());
    }

    private static String text(Cursor cursor, int column) {
        String value = cursor.getString(column);
        return value == null ? "" : value;
    }

    /**
     * Every IM row of protocol LXMF, with its contact: what a new phone's
     * address book says about the user's LXMF contacts (only with
     * READ_CONTACTS). The library links those it knows.
     */
    static List<Map<String, Object>> imRows(ContentResolver resolver) {
        List<Map<String, Object>> rows = new ArrayList<>();
        String[] columns = {Im.DATA, Data.CONTACT_ID, Data.LOOKUP_KEY};
        String where = Data.MIMETYPE + " = ? AND " + Im.PROTOCOL + " = ? AND UPPER("
                + Im.CUSTOM_PROTOCOL + ") = ?";
        String[] args = {Im.CONTENT_ITEM_TYPE, String.valueOf(Im.PROTOCOL_CUSTOM), LXMF};
        try (Cursor cursor = resolver.query(Data.CONTENT_URI, columns, where, args, null)) {
            while (cursor != null && cursor.moveToNext()) {
                Map<String, Object> copy = copyOf(resolver, cursor.getLong(1), text(cursor, 2));
                if (copy != null) {
                    copy.put("address", text(cursor, 0));
                    rows.add(copy);
                }
            }
        } catch (RuntimeException unreadable) {
            return rows;
        }
        return rows;
    }

    /**
     * Read every linked contact again (only with READ_CONTACTS): a new name
     * or photo is copied, a contact that is gone unlinks; then the IM rows
     * link what they name. Off the UI thread.
     */
    static void refresh(Context context) {
        if (!canRead(context)) {
            return;
        }
        ContentResolver resolver = context.getContentResolver();
        for (Map<String, Object> link : Core.links()) {
            String peer = Core.str(link, "peer");
            Map<String, Object> copy = copyOf(resolver, Core.num(link, "contact_id"),
                    Core.str(link, "lookup"));
            if (copy == null) {
                Core.unlink(peer);
            } else {
                Core.link(peer, copy);
            }
        }
        Core.relink(imRows(resolver));
    }

    /**
     * Watch the address book while READ_CONTACTS is granted: a change,
     * once it settled, refreshes the links. Once per process.
     */
    static synchronized void observe(final Context context) {
        if (observer != null || !canRead(context)) {
            return;
        }
        final Context app = context.getApplicationContext();
        final Handler main = new Handler(Looper.getMainLooper());
        final Runnable run = new Runnable() {
            @Override
            public void run() {
                new Thread(new Runnable() {
                    @Override
                    public void run() {
                        refresh(app);
                    }
                }, "loquax-book").start();
            }
        };
        observer = new ContentObserver(main) {
            @Override
            public void onChange(boolean self) {
                main.removeCallbacks(run);
                main.postDelayed(run, SETTLE_MS);
            }
        };
        app.getContentResolver().registerContentObserver(Contacts.CONTENT_URI, true, observer);
        // What changed while nobody watched.
        main.postDelayed(run, SETTLE_MS);
    }

    /** The address of an `imto://lxmf/<hex>` the contacts app's IM row sent, or null. */
    static String imtoPeer(Intent intent) {
        Uri data = intent == null ? null : intent.getData();
        if (data == null || !"imto".equalsIgnoreCase(data.getScheme())
                || !"lxmf".equalsIgnoreCase(data.getHost())) {
            return null;
        }
        String hex = data.getLastPathSegment();
        if (hex == null) {
            return null;
        }
        hex = hex.trim().toLowerCase(java.util.Locale.ROOT);
        return hex.matches("[0-9a-f]{32}") ? hex : null;
    }
}
