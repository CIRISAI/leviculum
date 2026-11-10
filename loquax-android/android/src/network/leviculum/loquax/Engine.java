package network.leviculum.loquax;

import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ThreadFactory;

/**
 * The thread the library's commands and questions run on (1446, 1464). A command waits on the
 * session's lock, which the library's pump holds while it folds what the
 * engine said; the first pull of a big mailbox kept it for 12 s, and
 * `ConversationsActivity.onResume` waited for it on the UI thread until
 * Android said the app was not responding. So a screen never calls
 * {@link Native#command} itself: it hands the call here and gets the
 * answer back on the UI thread. One thread, so the commands reach the
 * library in the order the screens gave them.
 *
 * <p>The guard, {@link #offMain}: in a debuggable build a blocking native
 * call on the UI thread throws {@link OnMainThread}, so a call site that
 * slips back onto the UI thread fails at once on the developer's phone
 * instead of freezing a user's.
 */
final class Engine {
    /** A library call; its error in words, or null. */
    interface Call {
        String run();
    }

    /** What a {@link Call} said, on the UI thread. */
    interface Said {
        void said(String error);
    }

    /** A library question (1464); its answer. */
    interface Ask<T> {
        T ask();
    }

    /** What an {@link Ask} answered, on the UI thread. */
    interface Got<T> {
        void got(T answer);
    }

    /** A blocking native call on the UI thread, in a debuggable build. */
    static final class OnMainThread extends IllegalStateException {
        OnMainThread(String what) {
            super(what + " on the UI thread: it waits on the session's lock;"
                    + " hand it to Engine.run (1446)");
        }
    }

    private static final String TAG = "loquax";
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final ExecutorService THREAD = Executors.newSingleThreadExecutor(
            new ThreadFactory() {
                @Override
                public Thread newThread(Runnable runnable) {
                    Thread thread = new Thread(runnable, "loquax-engine");
                    thread.setDaemon(true);
                    return thread;
                }
            });
    /** Whether {@link #offMain} throws: set by {@link LoquaxApp} in a debuggable build. */
    static volatile boolean strict;

    private Engine() {}

    /** Run `call` on the engine's thread; an error goes to the log. */
    static void run(final Call call) {
        run(call, null);
    }

    /** Run `call` on the engine's thread; what it said to `said` on the UI thread. */
    static void run(final Call call, final Said said) {
        THREAD.execute(new Runnable() {
            @Override
            public void run() {
                final String error = call.run();
                if (said == null) {
                    if (error != null) {
                        Log.w(TAG, "a command said: " + error);
                    }
                    return;
                }
                MAIN.post(new Runnable() {
                    @Override
                    public void run() {
                        said.said(error);
                    }
                });
            }
        });
    }

    /**
     * Ask `ask` on the engine's thread (1464), behind the commands handed
     * before it; its answer to `got` on the UI thread. A question waits on
     * the session's lock as a command does: the Pixel spent 83.6 % of a
     * first pull's 44 s in the list's query on the UI thread.
     */
    static <T> void ask(final Ask<T> ask, final Got<T> got) {
        ask(ask, got, null);
    }

    /**
     * {@link #ask(Ask, Got)}, and `failed` on the UI thread when the
     * question threw (1496): a reply the library could not make, a
     * MsgPack that did not decode, memory that ran out. Before, a throw
     * left the caller waiting for `got` forever; the chat's `asking`
     * stayed set and it never asked again. The throw goes to the log.
     */
    static <T> void ask(final Ask<T> ask, final Got<T> got, final Runnable failed) {
        THREAD.execute(new Runnable() {
            @Override
            public void run() {
                final T answer;
                try {
                    answer = ask.ask();
                } catch (Throwable thrown) {
                    Log.e(TAG, "a question threw", thrown);
                    if (failed != null) {
                        MAIN.post(failed);
                    }
                    return;
                }
                MAIN.post(new Runnable() {
                    @Override
                    public void run() {
                        got.got(answer);
                    }
                });
            }
        });
    }

    /**
     * The guard: `what`, a call that waits on the session, is not on the UI
     * thread. Throws {@link OnMainThread} there in a debuggable build; a
     * release build logs it and goes on.
     */
    static void offMain(String what) {
        if (Looper.myLooper() != Looper.getMainLooper()) {
            return;
        }
        OnMainThread caught = new OnMainThread(what);
        if (strict) {
            throw caught;
        }
        Log.e(TAG, caught.getMessage(), caught);
    }
}
