package network.leviculum.loquax;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.graphics.Bitmap;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;

/**
 * The profile's QR code as a picture for the share sheet (1187): one PNG
 * in the cache, served read only to the app the user picks, which the
 * share intent grants it. AndroidX's FileProvider does this for any file;
 * this app shares exactly one, so it needs no more than this.
 */
public class QrProvider extends ContentProvider {
    private static final String NAME = "loquax-contact.png";

    /** The picture of `bitmap` written and its content URI; null when it cannot be written. */
    static Uri write(Context context, Bitmap bitmap) {
        File file = file(context);
        try (FileOutputStream out = new FileOutputStream(file)) {
            if (!bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)) {
                return null;
            }
        } catch (IOException e) {
            return null;
        }
        return Uri.parse("content://" + context.getPackageName() + ".qr/" + NAME);
    }

    private static File file(Context context) {
        return new File(context.getCacheDir(), NAME);
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!NAME.equals(uri.getLastPathSegment()) || !"r".equals(mode)) {
            throw new FileNotFoundException(uri.toString());
        }
        return ParcelFileDescriptor.open(file(getContext()), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public String getType(Uri uri) {
        return "image/png";
    }

    /** The name and size a receiving app asks for before it reads. */
    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String order) {
        MatrixCursor cursor = new MatrixCursor(
                new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE});
        cursor.addRow(new Object[] {NAME, file(getContext()).length()});
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        return null;
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        return 0;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        return 0;
    }
}
