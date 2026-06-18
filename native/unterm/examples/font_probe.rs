//! Inspect how candidate monospace fonts register in the font DB.

use glyphon::FontSystem;

fn main() {
    let mut fs = FontSystem::new();
    for path in [
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Monaco.ttf",
    ] {
        let before = fs.db().faces().count();
        if fs.db_mut().load_font_file(path).is_err() {
            println!("FAILED: {path}");
            continue;
        }
        println!("== {path} ==");
        let faces: Vec<_> = fs.db().faces().skip(before).collect();
        for f in faces {
            let fam = f
                .families
                .first()
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            println!(
                "  family={:?} weight={:?} style={:?} monospaced={}",
                fam, f.weight, f.style, f.monospaced
            );
        }
    }
}
