package network.leviculum.loquax;

import android.app.Application;
import android.content.pm.ApplicationInfo;

/**
 * The process's first code (1446): in a debuggable build it makes the
 * guard of {@link Engine#offMain} throw, before any screen or the service
 * can call the library on the UI thread.
 */
public final class LoquaxApp extends Application {
    @Override
    public void onCreate() {
        super.onCreate();
        Engine.strict = (getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
    }
}
