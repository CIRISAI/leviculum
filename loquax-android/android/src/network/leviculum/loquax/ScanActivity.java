package network.leviculum.loquax;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.graphics.SurfaceTexture;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Size;
import android.view.Gravity;
import android.view.Surface;
import android.view.TextureView;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.TextView;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.Map;

/**
 * The scanner (1152): the back camera's preview in a plain TextureView,
 * and every frame's luminance plane to the library's decoder
 * (loquax-android/src/scan.rs), one frame at a time, until a code of the
 * kind asked for shows. Camera2 only: no AndroidX, no Google service.
 *
 * Started for a result; the code's text comes back as {@link #TEXT}.
 * {@link #PREFIX} says what is wanted: `lqxh1` for a mailbox's join code,
 * `lqxb1` for a paper backup; several, split by `|`, take any of them;
 * {@link #CODES} takes every code a friend or a mailbox shows, as the
 * library reads it (loquax::code, 1263): a contact code, an address alone,
 * a mailbox's code. Another code in view is not ignored: the line at the
 * bottom says in one sentence why it is not taken, and the scan goes on.
 */
public class ScanActivity extends Activity {
    static final String PREFIX = "prefix";
    /** The {@link #PREFIX} for any code a friend or a mailbox shows. */
    static final String CODES = "codes";
    static final String TEXT = "text";
    private static final int ASK_CAMERA = 1;
    /** The largest frame asked for: enough for a code on a screen or paper. */
    private static final int MAX_SIDE = 1280;

    private TextureView preview;
    private TextView said;
    private String[] prefixes;
    private HandlerThread worker;
    private Handler background;
    private CameraDevice camera;
    private CameraCaptureSession session;
    private ImageReader reader;
    private volatile boolean decoding;
    private volatile boolean found;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        String prefix = getIntent().getStringExtra(PREFIX);
        prefixes = (prefix == null ? "" : prefix).split("\\|", -1);
        FrameLayout frame = new FrameLayout(this);
        frame.setBackgroundColor(0xff000000);
        preview = new TextureView(this);
        frame.addView(preview, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        said = new TextView(this);
        said.setTextColor(0xffffffff);
        said.setTextSize(16);
        said.setBackgroundColor(0x99000000);
        said.setPadding(32, 24, 32, 24);
        said.setText(R.string.scan_hint);
        frame.addView(said, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM));
        setContentView(frame);
    }

    @Override
    protected void onResume() {
        super.onResume();
        worker = new HandlerThread("loquax-scan");
        worker.start();
        background = new Handler(worker.getLooper());
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.CAMERA}, ASK_CAMERA);
            return;
        }
        whenShown();
    }

    @Override
    protected void onPause() {
        close();
        if (worker != null) {
            worker.quitSafely();
            worker = null;
        }
        super.onPause();
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        if (request != ASK_CAMERA) {
            return;
        }
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            whenShown();
        } else {
            said.setText(R.string.scan_no_camera);
        }
    }

    /** Open the camera once the preview has a surface. */
    private void whenShown() {
        if (preview.isAvailable()) {
            open();
            return;
        }
        preview.setSurfaceTextureListener(new TextureView.SurfaceTextureListener() {
            @Override
            public void onSurfaceTextureAvailable(SurfaceTexture texture, int w, int h) {
                open();
            }

            @Override
            public void onSurfaceTextureSizeChanged(SurfaceTexture texture, int w, int h) {}

            @Override
            public boolean onSurfaceTextureDestroyed(SurfaceTexture texture) {
                return true;
            }

            @Override
            public void onSurfaceTextureUpdated(SurfaceTexture texture) {}
        });
    }

    @SuppressWarnings("MissingPermission")
    private void open() {
        CameraManager cameras = getSystemService(CameraManager.class);
        try {
            String id = backCamera(cameras);
            if (id == null) {
                said.setText(R.string.scan_no_camera);
                return;
            }
            StreamConfigurationMap formats = cameras.getCameraCharacteristics(id)
                    .get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            final Size size = frameSize(formats.getOutputSizes(ImageFormat.YUV_420_888));
            reader = ImageReader.newInstance(size.getWidth(), size.getHeight(),
                    ImageFormat.YUV_420_888, 2);
            reader.setOnImageAvailableListener(new ImageReader.OnImageAvailableListener() {
                @Override
                public void onImageAvailable(ImageReader from) {
                    frame(from);
                }
            }, background);
            cameras.openCamera(id, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice device) {
                    camera = device;
                    capture(size);
                }

                @Override
                public void onDisconnected(CameraDevice device) {
                    device.close();
                    camera = null;
                }

                @Override
                public void onError(CameraDevice device, int error) {
                    device.close();
                    camera = null;
                    failed();
                }
            }, background);
        } catch (CameraAccessException | RuntimeException refused) {
            failed();
        }
    }

    /** The preview and the decoder's reader, both fed by one repeating request. */
    private void capture(Size size) {
        try {
            SurfaceTexture texture = preview.getSurfaceTexture();
            texture.setDefaultBufferSize(size.getWidth(), size.getHeight());
            final Surface shown = new Surface(texture);
            final Surface read = reader.getSurface();
            camera.createCaptureSession(Arrays.asList(shown, read),
                    new CameraCaptureSession.StateCallback() {
                        @Override
                        public void onConfigured(CameraCaptureSession configured) {
                            session = configured;
                            try {
                                CaptureRequest.Builder request = camera.createCaptureRequest(
                                        CameraDevice.TEMPLATE_PREVIEW);
                                request.addTarget(shown);
                                request.addTarget(read);
                                request.set(CaptureRequest.CONTROL_AF_MODE,
                                        CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE);
                                configured.setRepeatingRequest(request.build(), null, background);
                            } catch (CameraAccessException | RuntimeException refused) {
                                failed();
                            }
                        }

                        @Override
                        public void onConfigureFailed(CameraCaptureSession configured) {
                            failed();
                        }
                    }, background);
        } catch (CameraAccessException | RuntimeException refused) {
            failed();
        }
    }

    /** One frame: its luminance to the decoder, skipped while the last one is still read. */
    private void frame(ImageReader from) {
        Image image = from.acquireLatestImage();
        if (image == null) {
            return;
        }
        if (decoding || found) {
            image.close();
            return;
        }
        decoding = true;
        final int width = image.getWidth();
        final int height = image.getHeight();
        Image.Plane luma = image.getPlanes()[0];
        final int stride = luma.getRowStride();
        ByteBuffer buffer = luma.getBuffer();
        final byte[] bytes = new byte[buffer.remaining()];
        buffer.get(bytes);
        image.close();
        String text = Native.scan(bytes, width, height, stride);
        if (text == null || text.isEmpty()) {
            decoding = false;
            return;
        }
        final String why = refusal(text);
        decoding = false;
        if (why != null) {
            runOnUiThread(new Runnable() {
                @Override
                public void run() {
                    if (!found && !why.contentEquals(said.getText())) {
                        said.setText(why);
                    }
                }
            });
            return;
        }
        found = true;
        final String code = text;
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                setResult(RESULT_OK, new Intent().putExtra(TEXT, code));
                finish();
            }
        });
    }

    /**
     * Why `text` is not taken here, in one sentence; null when it is: in
     * {@link #CODES} what the library says of it, else whether it begins
     * with one of the prefixes asked for, in any case.
     */
    private String refusal(String text) {
        if (prefixes.length == 1 && CODES.equals(prefixes[0])) {
            Map<String, Object> read = Core.code(text);
            return read.containsKey("error") ? Core.str(read, "error") : null;
        }
        for (String prefix : prefixes) {
            if (text.regionMatches(true, 0, prefix, 0, prefix.length())) {
                return null;
            }
        }
        Map<String, Object> read = Core.code(text);
        return read.containsKey("error") ? Core.str(read, "error") : getString(R.string.scan_wrong_kind);
    }

    private void failed() {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                said.setText(R.string.scan_no_camera);
            }
        });
    }

    private void close() {
        if (session != null) {
            session.close();
            session = null;
        }
        if (camera != null) {
            camera.close();
            camera = null;
        }
        if (reader != null) {
            reader.close();
            reader = null;
        }
    }

    private static String backCamera(CameraManager cameras) throws CameraAccessException {
        String first = null;
        for (String id : cameras.getCameraIdList()) {
            Integer facing = cameras.getCameraCharacteristics(id)
                    .get(CameraCharacteristics.LENS_FACING);
            if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                return id;
            }
            if (first == null) {
                first = id;
            }
        }
        return first;
    }

    /** The largest offered size whose long side is at most {@link #MAX_SIDE}. */
    private static Size frameSize(Size[] offered) {
        Size best = null;
        for (Size size : offered) {
            if (Math.max(size.getWidth(), size.getHeight()) > MAX_SIDE) {
                continue;
            }
            if (best == null || size.getWidth() * size.getHeight()
                    > best.getWidth() * best.getHeight()) {
                best = size;
            }
        }
        return best != null ? best : offered[0];
    }

    /** Whether this phone has a camera to scan with. */
    static boolean available(Context context) {
        return context.getPackageManager().hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY);
    }
}
