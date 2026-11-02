//! The app's icon, the jackdaw (1336), as the tree carries it: a legacy
//! `ic_launcher.png` for every mipmap density at its size, the adaptive
//! icon over its three layers, the manifest naming it, and the Linux
//! package's 256 px PNG. `scripts/render-icon.py` writes all of them; its
//! `--check` holds them to the script's bytes, this test to what Android
//! and the package need.

use std::path::{Path, PathBuf};

fn tree(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The PNG's width, height and colour type.
fn decode(path: &Path) -> (u32, u32, png::ColorType) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader
        .next_frame(&mut buf)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (info.width, info.height, info.color_type)
}

/// 48 dp at each density's scale: 1, 1.5, 2, 3 and 4.
#[test]
fn every_density_has_its_launcher_png_at_its_size() {
    for (density, px) in [
        ("mdpi", 48),
        ("hdpi", 72),
        ("xhdpi", 96),
        ("xxhdpi", 144),
        ("xxxhdpi", 192),
    ] {
        let path = tree(&format!("android/res/mipmap-{density}/ic_launcher.png"));
        let (w, h, colour) = decode(&path);
        assert_eq!((w, h), (px, px), "{}", path.display());
        assert_eq!(colour, png::ColorType::Rgba, "{}", path.display());
    }
}

#[test]
fn the_adaptive_icon_has_its_layers_and_the_manifest_names_it() {
    let adaptive = read(&tree("android/res/mipmap-anydpi-v26/ic_launcher.xml"));
    for layer in [
        "<background a:drawable='@color/ic_launcher_background'/>",
        "<foreground a:drawable='@drawable/ic_launcher_foreground'/>",
        "<monochrome a:drawable='@drawable/ic_launcher_monochrome'/>",
    ] {
        assert!(adaptive.contains(layer), "the adaptive icon lacks {layer}");
    }
    for drawable in ["ic_launcher_foreground", "ic_launcher_monochrome"] {
        let xml = read(&tree(&format!("android/res/drawable/{drawable}.xml")));
        assert!(
            xml.contains("<vector"),
            "{drawable} is not a vector drawable"
        );
    }
    let colour = read(&tree("android/res/values/ic_launcher.xml"));
    assert!(colour.contains("<color name=\"ic_launcher_background\">"));

    let manifest = read(&tree("android/AndroidManifest.xml"));
    assert!(manifest.contains("a:icon='@mipmap/ic_launcher'"));
    assert!(manifest.contains("a:roundIcon='@mipmap/ic_launcher'"));
    assert!(!manifest.contains("sym_def_app_icon"));
}

#[test]
fn the_linux_package_has_a_256_px_icon_and_a_launcher_entry() {
    let (w, h, _) = decode(&tree("../packaging/loquax/loquax.png"));
    assert_eq!((w, h), (256, 256));
    // The window's entry (amd64, arm64) and the terminal's (armhf, whose
    // build carries no window), both installed as loquax.desktop (1342).
    let entry = read(&tree("../packaging/loquax/loquax.desktop"));
    assert!(entry.contains("\nIcon=loquax\n"));
    assert!(entry.contains("\nExec=loquax gui\n"));
    assert!(entry.contains("\nTerminal=false\n"));
    // The window's WM_CLASS and Wayland app id, `desktop::APP`.
    assert!(entry.contains("\nStartupWMClass=loquax\n"));
    let terminal = read(&tree("../packaging/loquax/loquax-terminal.desktop"));
    assert!(terminal.contains("\nIcon=loquax\n"));
    assert!(terminal.contains("\nExec=loquax tui\n"));
    assert!(terminal.contains("\nTerminal=true\n"));
}
