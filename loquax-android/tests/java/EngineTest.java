package network.leviculum.loquax;

import android.os.Handler;
import android.os.Looper;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

/**
 * The engine's questions on the host (1496), run by tests/fling.rs with
 * stand-ins for Android's Handler, Looper and Log: an answer comes to
 * `got` on the UI thread; a question that throws comes to `failed` there,
 * so a chat's `asking` clears; the thread answers the next question after
 * a throw. Before 1496 a throw left `got` unsaid and nothing else, and the
 * chat never asked again.
 */
final class EngineTest {
    private static int checks;

    public static void main(String[] args) throws Exception {
        Looper.becomeMain();
        final AtomicReference<String> got = new AtomicReference<>();
        final AtomicInteger failed = new AtomicInteger();

        Engine.ask(new Engine.Ask<String>() {
            @Override
            public String ask() {
                return "the chat";
            }
        }, new Engine.Got<String>() {
            @Override
            public void got(String answer) {
                got.set(answer);
            }
        }, new Runnable() {
            @Override
            public void run() {
                failed.incrementAndGet();
            }
        });
        check(Handler.runNext(5000), "an answer is posted");
        check("the chat".equals(got.get()), "its answer to got: " + got.get());
        check(failed.get() == 0, "no failure for an answer");

        got.set(null);
        Engine.ask(new Engine.Ask<String>() {
            @Override
            public String ask() {
                throw new IllegalStateException("a reply that does not decode");
            }
        }, new Engine.Got<String>() {
            @Override
            public void got(String answer) {
                got.set("got after a throw");
            }
        }, new Runnable() {
            @Override
            public void run() {
                failed.incrementAndGet();
            }
        });
        check(Handler.runNext(5000), "a throw posts its failure");
        check(failed.get() == 1, "failed ran once: " + failed.get());
        check(got.get() == null, "got is not said after a throw");

        // Without a failure path the throw is logged, and the thread still
        // answers the next question.
        Engine.ask(new Engine.Ask<String>() {
            @Override
            public String ask() {
                throw new OutOfMemoryError("a chat too large");
            }
        }, new Engine.Got<String>() {
            @Override
            public void got(String answer) {
                got.set("got after a throw");
            }
        });
        Engine.ask(new Engine.Ask<String>() {
            @Override
            public String ask() {
                return "after";
            }
        }, new Engine.Got<String>() {
            @Override
            public void got(String answer) {
                got.set(answer);
            }
        });
        check(Handler.runNext(5000), "the question after a throw is answered");
        check("after".equals(got.get()), "the next answer: " + got.get());
        check(!Handler.runNext(200), "nothing else was posted");

        System.out.println(checks + " checks passed");
    }

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) {
            throw new AssertionError(what);
        }
    }
}
