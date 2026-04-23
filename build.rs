fn main() {
    // Icon embedding is Windows-only.
    // On macOS distribute as a .app bundle (cargo-bundle).
    // On Linux provide a .desktop file.
    #[cfg(target_os = "windows")]
    embed_windows_icon();
}

#[cfg(target_os = "windows")]
fn embed_windows_icon() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let ico_path = std::path::PathBuf::from(&out_dir).join("ips_gui.ico");
    svg_to_ico(include_str!("icon.svg"), &ico_path);

    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_str().unwrap());
    res.compile().unwrap();
}

#[cfg(target_os = "windows")]
fn svg_to_ico(svg: &str, out: &std::path::Path) {
    use resvg::{tiny_skia, usvg};

    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opts).expect("failed to parse icon.svg");
    let orig_w = tree.size().width();
    let orig_h = tree.size().height();

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &sz in &[16u32, 32, 48, 256] {
        let tf = tiny_skia::Transform::from_scale(sz as f32 / orig_w, sz as f32 / orig_h);
        let mut pixmap = tiny_skia::Pixmap::new(sz, sz).unwrap();
        resvg::render(&tree, tf, &mut pixmap.as_mut());
        let img = ico::IconImage::from_rgba_data(sz, sz, pixmap.data().to_vec());
        dir.add_entry(ico::IconDirEntry::encode(&img).unwrap());
    }
    dir.write(std::fs::File::create(out).unwrap()).unwrap();
}
