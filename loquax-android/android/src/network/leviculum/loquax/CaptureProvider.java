package network.leviculum.loquax;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import java.io.File;
import java.io.FileNotFoundException;

/**
 * The one file the camera app writes the compose bar's photo into (1270):
 * files/capture/photo.jpg, writable by the app the capture intent grants
 * it to, and nothing else. FilesProvider serves received files read only;
 * this is its writing twin for exactly one path.
 */
public class CaptureProvider extends ContentProvider {
    private static final String DIR = "capture";
    private static final String NAME = "photo.jpg";

    /** The capture's file, its folder made. */
    static File file(Context context) {
        File dir = new File(context.getFilesDir(), DIR);
        if (!dir.isDirectory()) {
            dir.mkdirs();
        }
        return new File(dir, NAME);
    }

    /** The URI the capture intent writes to. */
    static Uri uri(Context context) {
        return new Uri.Builder().scheme("content")
                .authority(context.getPackageName() + ".capture")
                .appendPath(NAME)
                .build();
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!NAME.equals(uri.getLastPathSegment()) || uri.getPathSegments().size() != 1) {
            throw new FileNotFoundException(uri.toString());
        }
        return ParcelFileDescriptor.open(file(getContext()), ParcelFileDescriptor.parseMode(mode)
                | ParcelFileDescriptor.MODE_CREATE);
    }

    @Override
    public String getType(Uri uri) {
        return "image/jpeg";
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String order) {
        return null;
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
