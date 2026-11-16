//! No screen calls the library's commands on the UI thread (1446). UI
//! walk 1 saw "Loquax reagiert nicht" right after the join:
//! `ConversationsActivity.onResume` called `Core.close()`, which waits on
//! the session's lock, while the pump held it to fold the first pull. The
//! device's guard (`Engine.offMain`, armed by `LoquaxApp` in a debuggable
//! build) throws on the phone; this is its host half, which needs no phone:
//! every call of a command builder of `Core.java` (a `static String` method
//! that goes through `command(`), and of `Locator.tell` and
//! `Locator.share`, which do too, sits in the `run()` of an
//! `Engine.Call`, or is one of the few calls a worker thread of its own
//! makes, each named below with the method it runs in. A call put back
//! onto the UI thread fails here by file and line.
//!
//! The library's questions too (1464): a fresh join still froze the Pixel
//! for 35 s, its UI thread 83.6 % of the time in `Native.query` for the
//! conversation list, waiting on the same lock. Every query builder of
//! `Core.java` (a `static` method that goes through `query(`) sits in the
//! `ask()` of an `Engine.Ask`, in an `Engine.Call`, or in a named worker.
//! The two lists the library answers without the lock (`Core.look`) and
//! the photo viewer's zoom, which asks no session, are the UI thread's.

use std::path::{Path, PathBuf};

/// The calls made off the UI thread by a worker of their own, not the
/// engine's, and the one inside a command of its own: the file, the call,
/// and the head of the method it runs in.
const WORKERS: &[(&str, &str, &str)] = &[
    // Address book refresh on "loquax-book".
    (
        "Book.java",
        "Core.unlink(",
        "static void refresh(Context context) {",
    ),
    (
        "Book.java",
        "Core.link(",
        "static void refresh(Context context) {",
    ),
    (
        "Book.java",
        "Core.relink(",
        "static void refresh(Context context) {",
    ),
    // The picked media, read and handed over on the chat's executors.
    ("ChatActivity.java", "Core.voice(", "public void run() {"),
    ("ChatActivity.java", "Core.stage(", "public void run() {"),
    ("ChatActivity.java", "Core.photo(", "public void run() {"),
    ("ChatActivity.java", "Core.file(", "public void run() {"),
    // The name, on "loquax-name"; the regions, on a thread of their own.
    ("ProfileActivity.java", "Core.name(", "public void run() {"),
    ("WelcomeActivity.java", "Core.name(", "public void run() {"),
    (
        "SettingsActivity.java",
        "Core.deleteRegions(",
        "public void run() {",
    ),
    // Questions on workers of their own (1464): the address book's links on
    // "loquax-book"; the chat's photos, voices and files on its executors;
    // the photo viewer's picture, the map's settings and the first start's
    // own address on threads of their own.
    (
        "Book.java",
        "Core.links(",
        "static void refresh(Context context) {",
    ),
    (
        "ChatActivity.java",
        "Core.stagedBitmap(",
        "public void run() {",
    ),
    ("ChatActivity.java", "Core.voicePcm(", "public void run() {"),
    ("ChatActivity.java", "Core.saveFile(", "public void run() {"),
    (
        "ChatActivity.java",
        "Core.photoBitmap(",
        "public void run() {",
    ),
    (
        "PhotoActivity.java",
        "Core.photoBitmap(",
        "public void run() {",
    ),
    (
        "SettingsActivity.java",
        "Core.mapTheme(",
        "public void run() {",
    ),
    (
        "SettingsActivity.java",
        "Core.mapRegions(",
        "public void run() {",
    ),
    (
        "SettingsActivity.java",
        "Core.mapFling(",
        "public void run() {",
    ),
    ("WelcomeActivity.java", "Core.me(", "public void run() {"),
    // Not a thread: `Locator.tell` is a command itself, checked at its callers.
    (
        "Locator.java",
        "Core.noFix(",
        "static String tell(Location fix) {",
    ),
];

fn sources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("android/src/network/leviculum/loquax")
}

/// The command builders of `Core.java`: its `static String` methods whose
/// body calls `command(`.
fn builders(core: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    for line in core.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("static ") && trimmed.ends_with('{') && trimmed.contains('(') {
            current = trimmed
                .strip_prefix("static String ")
                .and_then(|rest| rest.split('(').next())
                .map(str::to_string);
            continue;
        }
        if let Some(name) = &current {
            if line.contains("command(") && name != "command" && !found.contains(name) {
                found.push(name.clone());
            }
        }
    }
    found
}

/// The query builders of `Core.java` (1464): its `static` methods, of any
/// type, whose body calls `query(`, the guarded one, and not `query`
/// itself.
fn questions(core: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    for line in core.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("static ") && trimmed.ends_with('{') && trimmed.contains('(') {
            current = trimmed
                .split('(')
                .next()
                .and_then(|head| head.split_whitespace().last())
                .map(str::to_string);
            continue;
        }
        let code = line.split("//").next().unwrap_or_default();
        let guarded = code.contains("query(") && !code.contains("Native.query(");
        if let Some(name) = &current {
            if guarded && name != "query" && !found.contains(name) {
                found.push(name.clone());
            }
        }
    }
    found
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The head of the innermost method around line `at`: the nearest line
/// above, less indented, that opens a method.
fn enclosing(lines: &[&str], at: usize) -> Option<String> {
    let mut depth = indent(lines[at]);
    for line in lines[..at].iter().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || indent(line) >= depth {
            continue;
        }
        depth = indent(line);
        let opens = trimmed.ends_with('{')
            && trimmed.contains('(')
            && [
                "public ",
                "private ",
                "protected ",
                "static ",
                "void ",
                "String ",
            ]
            .iter()
            .any(|head| trimmed.starts_with(head));
        if opens {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Every call in `source` (named `file`) of `calls` outside an
/// `Engine.Call` and not a named worker's: "file:line call".
fn on_the_ui_thread(file: &str, source: &str, calls: &[String]) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut wrong = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        if code.trim_start().starts_with('*') {
            continue;
        }
        for call in calls.iter().filter(|call| code.contains(call.as_str())) {
            let head = enclosing(&lines, at).unwrap_or_default();
            let engine = head == "public String run() {"
                || (head.starts_with("public ") && head.ends_with(" ask() {"));
            let worker = WORKERS
                .iter()
                .any(|(f, c, h)| *f == file && c == call && head == *h);
            if !engine && !worker {
                wrong.push(format!("{file}:{} {} (in `{head}`)", at + 1, line.trim()));
            }
        }
    }
    wrong
}

fn calls() -> Vec<String> {
    let core = std::fs::read_to_string(sources().join("Core.java")).expect("Core.java");
    let mut calls: Vec<String> = builders(&core)
        .into_iter()
        .chain(questions(&core))
        .map(|name| format!("Core.{name}("))
        .collect();
    calls.push("Locator.tell(".to_string());
    calls.push("Locator.share(".to_string());
    calls
}

#[test]
fn no_command_runs_on_the_ui_thread() {
    let calls = calls();
    // The builders are read, not listed: the walk's one and a few more.
    for known in ["Core.close(", "Core.front(", "Core.send(", "Core.group("] {
        assert!(
            calls.iter().any(|c| c == known),
            "{known} not found among {calls:?}"
        );
    }
    assert!(
        calls.len() >= 40,
        "only {} builders read: {calls:?}",
        calls.len()
    );
    // The questions too (1464): the Pixel's two and a few more; the lists
    // the library answers without the lock are not among them.
    for known in [
        "Core.messages(",
        "Core.detail(",
        "Core.bar(",
        "Core.findConversations(",
        "Core.friends(",
    ] {
        assert!(
            calls.iter().any(|c| c == known),
            "{known} not found among {calls:?}"
        );
    }
    for free in [
        "Core.conversations(",
        "Core.archived(",
        "Core.code(",
        "Core.zoom(",
    ] {
        assert!(
            !calls.iter().any(|c| c == free),
            "{free} waits on no lock (1464), yet it is among {calls:?}"
        );
    }
    let mut wrong = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(sources())
        .expect("the Java sources")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
        .collect();
    files.sort();
    for path in files {
        let file = path
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        if file == "Core.java" {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a source");
        wrong.extend(on_the_ui_thread(&file, &source, &calls));
    }
    assert!(
        wrong.is_empty(),
        "commands on the UI thread (1446), hand them to Engine.run:\n{}",
        wrong.join("\n")
    );
}

/// The device guard sits where every command passes: `Core.command` and
/// `Core.start` ask `Engine.offMain` before the native call, and the app
/// arms it in a debuggable build.
#[test]
fn the_device_guard_sits_before_every_native_command() {
    let read = |name: &str| std::fs::read_to_string(sources().join(name)).expect(name);
    let core = read("Core.java");
    for (head, native) in [
        ("static String command(", "Native.command("),
        ("static Map<String, Object> start(", "Native.start("),
        ("static Map<String, Object> query(", "Native.query("),
    ] {
        let body = core.split(head).nth(1).expect(head);
        let guard = body.find("Engine.offMain(").expect("the guard");
        let call = body.find(native).expect(native);
        assert!(guard < call, "{head}: the guard comes after {native}");
    }
    let app = read("LoquaxApp.java");
    assert!(app.contains("Engine.strict =") && app.contains("FLAG_DEBUGGABLE"));
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("android/AndroidManifest.xml"),
    )
    .expect("the manifest");
    assert!(manifest.contains("a:name='network.leviculum.loquax.LoquaxApp'"));
}

/// The positive control: the walk's own line, a bare `Core.close()` in
/// `onResume`, is caught; a worker's call outside its named method is
/// caught; the same calls through the engine pass.
#[test]
fn the_check_catches_the_walks_line() {
    let calls = calls();
    let bare = "\
    @Override
    protected void onResume() {
        super.onResume();
        if (Ui.NOT_STARTED.equals(Core.close())) {
            finish();
        }
    }
";
    let caught = on_the_ui_thread("ConversationsActivity.java", bare, &calls);
    assert_eq!(caught.len(), 1, "{caught:?}");
    assert!(
        caught[0].contains("ConversationsActivity.java:4"),
        "{caught:?}"
    );

    let moved = "\
    @Override
    protected void onResume() {
        super.onResume();
        Engine.run(new Engine.Call() {
            @Override
            public String run() {
                return Core.close();
            }
        });
    }
";
    assert!(on_the_ui_thread("ConversationsActivity.java", moved, &calls).is_empty());

    let stray = "\
    private void tap() {
        Core.voice(peer, pcm, rate);
    }
";
    assert_eq!(
        on_the_ui_thread("ChatActivity.java", stray, &calls).len(),
        1
    );
}

/// The positive control of 1464: the Pixel's own line, the chat's
/// `refresh` asking `Core.messages` on the UI thread, is caught; the same
/// question in an `Engine.Ask` passes; the list's `Core.conversations()`,
/// which the library answers without the lock, passes bare.
#[test]
fn the_check_catches_a_stray_query() {
    let calls = calls();
    let bare = "\
    private void refresh() {
        refresh(Core.messages(peer));
    }
";
    let caught = on_the_ui_thread("ChatActivity.java", bare, &calls);
    assert_eq!(caught.len(), 1, "{caught:?}");
    assert!(caught[0].contains("ChatActivity.java:2"), "{caught:?}");

    let asked = "\
    private void refresh() {
        Engine.ask(new Engine.Ask<Map<String, Object>>() {
            @Override
            public Map<String, Object> ask() {
                return Core.messages(of);
            }
        }, got);
    }
";
    assert!(on_the_ui_thread("ChatActivity.java", asked, &calls).is_empty());

    let look = "\
    private Map<String, Object> ask() {
        return archivedView ? Core.archived() : Core.conversations();
    }
";
    assert!(on_the_ui_thread("ConversationsActivity.java", look, &calls).is_empty());

    // A question a worker of its own asks outside its named method is caught.
    let moved = "\
    private void tap() {
        Core.photoBitmap(peer, key, edge);
    }
";
    assert_eq!(
        on_the_ui_thread("ChatActivity.java", moved, &calls).len(),
        1
    );
}
