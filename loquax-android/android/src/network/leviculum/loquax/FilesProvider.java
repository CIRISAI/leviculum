package network.leviculum.loquax;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.List;
import java.util.Locale;

/**
 * Received files for the app that opens or shares them (1221): the library
 * saves a bubble's file under the app's files/files/ (loquax::file_save),
 * one folder per message and file; this serves exactly that tree, read
 * only, to the app the user picks, which the intent grants it. AndroidX's
 * FileProvider does this for any file; this app needs no more than this.
 */
public class FilesProvider extends ContentProvider {
    /** The library's folder under the app's files (loquax::file_save::DIR). */
    private static final String DIR = "files";

    /** The content URI of `path`, a file the library saved; null for any other. */
    static Uri uri(Context context, String path) {
        File file = inside(context, new File(path));
        if (file == null) {
            return null;
        }
        return new Uri.Builder().scheme("content")
                .authority(context.getPackageName() + ".files")
                .appendPath(file.getParentFile().getName())
                .appendPath(file.getName())
                .build();
    }

    /** The type the other app is told, by the name's extension. */
    static String type(String name) {
        int dot = name.lastIndexOf('.');
        String ext = dot < 0 ? "" : name.substring(dot + 1).toLowerCase(Locale.ROOT);
        String type = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext);
        return type == null ? "application/octet-stream" : type;
    }

    private static File root(Context context) {
        return new File(context.getFilesDir(), DIR);
    }

    /** `file` if it is a file two levels under the root, else null. */
    private static File inside(Context context, File file) {
        try {
            File root = root(context).getCanonicalFile();
            File real = file.getCanonicalFile();
            File parent = real.getParentFile();
            if (parent == null || !root.equals(parent.getParentFile()) || !real.isFile()) {
                return null;
            }
            return real;
        } catch (IOException e) {
            return null;
        }
    }

    private File file(Uri uri) throws FileNotFoundException {
        List<String> parts = uri.getPathSegments();
        File file = parts.size() != 2 ? null : inside(getContext(),
                new File(new File(root(getContext()), parts.get(0)), parts.get(1)));
        if (file == null) {
            throw new FileNotFoundException(uri.toString());
        }
        return file;
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) {
            throw new FileNotFoundException(uri.toString());
        }
        return ParcelFileDescriptor.open(file(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public String getType(Uri uri) {
        return type(uri.getLastPathSegment() == null ? "" : uri.getLastPathSegment());
    }

    /** The name and size a receiving app asks for before it reads. */
    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String order) {
        File file;
        try {
            file = file(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        MatrixCursor cursor = new MatrixCursor(
                new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE});
        cursor.addRow(new Object[] {file.getName(), file.length()});
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
