#![windows_subsystem = "windows"]

mod ips;

use eframe::egui;
use ips::{
    discovery::discover_files,
    extract::extract_prompt,
    matcher::match_record,
    types::{Config, MatchMode, MatchResult, PromptRecord},
};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    time::Instant,
};

const GRID_THUMB: f32 = 100.0;
const GRID_GAP: f32 = 4.0;
const THUMB_LOAD_PX: u32 = 300;
const DETAIL_THUMB_MAX: f32 = 300.0;
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp"];
const THUMB_THREADS: usize = 4;

fn is_image(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn load_thumbnail(path: &PathBuf) -> Option<egui::ColorImage> {
    let img = image::open(path).ok()?;
    let img = img.thumbnail(THUMB_LOAD_PX, THUMB_LOAD_PX);
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

enum ThumbState {
    Loaded(egui::TextureHandle),
    Failed,
}

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Grid,
    Detail(usize),
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let candidates: &[&str] = &[
        // Windows
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msjh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Linux — Noto CJK (location varies by distro)
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/arphic/uming.ttc",
    ];

    for path in candidates {
        if let Ok(data) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(data));
            for list in fonts.families.values_mut() {
                list.push("cjk".to_owned());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("IPS — Image Prompt Search")
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "IPS GUI",
        options,
        Box::new(|cc| Ok(Box::new(IpsGuiApp::new(cc)))),
    )
}

enum SearchMsg {
    Done(Vec<MatchResult>, f64),
}

#[derive(PartialEq, Clone)]
enum MatchModeOpt {
    Exact,
    Fuzzy,
    Regex,
}

struct IpsGuiApp {
    query: String,
    search_path: String,
    match_mode: MatchModeOpt,
    min_score: i64,
    no_recursive: bool,
    verbose: bool,
    depth_str: String,
    search_within_results: bool,

    searching: bool,
    results: Vec<MatchResult>,
    status_msg: String,
    error_msg: Option<String>,
    rx: Option<Receiver<SearchMsg>>,

    view_mode: ViewMode,

    thumb_pool: rayon::ThreadPool,
    thumb_result_tx: Sender<(PathBuf, Option<egui::ColorImage>)>,
    thumb_rx: Receiver<(PathBuf, Option<egui::ColorImage>)>,
    thumb_queued: HashSet<PathBuf>,
    thumbnails: HashMap<PathBuf, ThumbState>,
}

impl IpsGuiApp {
    fn new(cc: &eframe::CreationContext) -> Self {
        setup_fonts(&cc.egui_ctx);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(THUMB_THREADS)
            .build()
            .unwrap();
        let (res_tx, res_rx) = mpsc::channel();

        Self {
            query: String::new(),
            search_path: ".".into(),
            match_mode: MatchModeOpt::Exact,
            min_score: 50,
            no_recursive: false,
            verbose: false,
            depth_str: String::new(),
            search_within_results: false,
            searching: false,
            results: Vec::new(),
            status_msg: "Enter a query and click Search.".into(),
            error_msg: None,
            rx: None,
            view_mode: ViewMode::Grid,
            thumb_pool: pool,
            thumb_result_tx: res_tx,
            thumb_rx: res_rx,
            thumb_queued: HashSet::new(),
            thumbnails: HashMap::new(),
        }
    }

    fn request_thumb(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.thumbnails.contains_key(&path) || self.thumb_queued.contains(&path) {
            return;
        }
        self.thumb_queued.insert(path.clone());
        let tx = self.thumb_result_tx.clone();
        let ctx2 = ctx.clone();
        self.thumb_pool.spawn(move || {
            let img = load_thumbnail(&path);
            let _ = tx.send((path, img));
            ctx2.request_repaint();
        });
    }

    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((path, img)) = self.thumb_rx.try_recv() {
            self.thumb_queued.remove(&path);
            let state = match img {
                Some(color_img) => {
                    let handle = ctx.load_texture(
                        path.to_string_lossy().as_ref(),
                        color_img,
                        egui::TextureOptions::LINEAR,
                    );
                    ThumbState::Loaded(handle)
                }
                None => ThumbState::Failed,
            };
            self.thumbnails.insert(path, state);
        }
    }

    fn start_search(&mut self, ctx: &egui::Context) {
        if self.query.is_empty() || self.searching {
            return;
        }

        if self.match_mode == MatchModeOpt::Regex {
            if let Err(e) = regex::Regex::new(&self.query) {
                self.error_msg = Some(format!("Invalid regex: {e}"));
                return;
            }
        }

        let config = Config {
            query: self.query.clone(),
            path: PathBuf::from(&self.search_path),
            match_mode: match self.match_mode {
                MatchModeOpt::Exact => MatchMode::Exact,
                MatchModeOpt::Fuzzy => MatchMode::Fuzzy,
                MatchModeOpt::Regex => MatchMode::Regex,
            },
            min_score: self.min_score,
            depth: self.depth_str.trim().parse().ok(),
            no_recursive: self.no_recursive,
            verbose: self.verbose,
        };

        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();

        if self.search_within_results && !self.results.is_empty() {
            // Filter the existing result set instead of scanning the filesystem.
            let records: Vec<PromptRecord> =
                self.results.iter().map(|r| r.record.clone()).collect();

            std::thread::spawn(move || {
                let t0 = Instant::now();
                let mut results: Vec<MatchResult> = records
                    .par_iter()
                    .filter_map(|rec| match_record(rec, &config))
                    .collect();
                results.sort_by(|a, b| a.record.path.cmp(&b.record.path));
                let elapsed = t0.elapsed().as_secs_f64();
                let _ = tx.send(SearchMsg::Done(results, elapsed));
                ctx2.request_repaint();
            });
        } else {
            self.thumbnails.clear();
            self.thumb_queued.clear();

            std::thread::spawn(move || {
                let t0 = Instant::now();
                let files = discover_files(&config);

                let mut results: Vec<MatchResult> = files
                    .par_iter()
                    .flat_map(|path| {
                        extract_prompt(path, config.verbose)
                            .into_iter()
                            .filter_map(|rec| match_record(&rec, &config))
                            .collect::<Vec<_>>()
                    })
                    .collect();

                results.sort_by(|a, b| a.record.path.cmp(&b.record.path));
                let elapsed = t0.elapsed().as_secs_f64();
                let _ = tx.send(SearchMsg::Done(results, elapsed));
                ctx2.request_repaint();
            });
        }

        self.searching = true;
        self.error_msg = None;
        self.status_msg = "Searching…".into();
        self.rx = Some(rx);
        self.view_mode = ViewMode::Grid;
    }

    fn export_json(&self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("results.json")
            .save_file()
        else {
            return;
        };

        use serde::Serialize;
        #[derive(Serialize)]
        struct Row<'a> {
            path: String,
            generator: String,
            prompt: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            score: Option<i64>,
        }

        let rows: Vec<Row> = self
            .results
            .iter()
            .map(|r| Row {
                path: r.record.path.display().to_string(),
                generator: r.record.generator.to_string(),
                prompt: &r.record.prompt,
                score: r.score,
            })
            .collect();

        if let Ok(json) = serde_json::to_string_pretty(&rows) {
            let _ = std::fs::write(path, json);
        }
    }

    fn export_csv(&self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("results.csv")
            .save_file()
        else {
            return;
        };

        if let Ok(mut wtr) = csv::Writer::from_path(path) {
            let _ = wtr.write_record(["path", "generator", "prompt", "score"]);
            for r in &self.results {
                let score_str = r.score.map(|s| s.to_string()).unwrap_or_default();
                let _ = wtr.write_record([
                    &r.record.path.display().to_string(),
                    &r.record.generator.to_string(),
                    &r.record.prompt,
                    &score_str,
                ]);
            }
        }
    }
}

impl eframe::App for IpsGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── Poll search thread ───────────────────────────────────────────────
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(SearchMsg::Done(results, secs)) => {
                    let n = results.len();
                    self.results = results;
                    self.searching = false;
                    self.rx = None;
                    self.status_msg = format!("Found {n} result(s) in {secs:.2}s");
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(80));
                }
                Err(TryRecvError::Disconnected) => {
                    self.searching = false;
                    self.rx = None;
                }
            }
        }

        self.poll_thumbs(ctx);

        // ── Left panel ───────────────────────────────────────────────────────
        egui::SidePanel::left("params")
            .exact_width(ctx.screen_rect().width() * 0.30)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.heading("Search Parameters");
                ui.separator();
                ui.add_space(8.0);

                ui.label("Query:");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("e.g. cyberpunk city")
                        .desired_width(ui.available_width()),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.start_search(ctx);
                }
                ui.add_space(10.0);

                ui.label("Directory:");
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 75.0;
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_path).desired_width(w),
                    );
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.search_path = p.display().to_string();
                        }
                    }
                });
                ui.add_space(10.0);

                ui.label("Match Mode:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.match_mode, MatchModeOpt::Exact, "Exact");
                    ui.selectable_value(&mut self.match_mode, MatchModeOpt::Fuzzy, "Fuzzy");
                    ui.selectable_value(&mut self.match_mode, MatchModeOpt::Regex, "Regex");
                });

                if self.match_mode == MatchModeOpt::Fuzzy {
                    ui.add_space(4.0);
                    ui.label(format!("Min Score: {}", self.min_score));
                    ui.add(egui::Slider::new(&mut self.min_score, 0..=100).show_value(false));
                }
                ui.add_space(10.0);

                ui.label("Max Depth (blank = unlimited):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.depth_str)
                        .hint_text("e.g. 3")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(10.0);

                ui.label("Options:");
                ui.checkbox(&mut self.no_recursive, "Top-level only (non-recursive)");
                if !self.results.is_empty() {
                    ui.checkbox(&mut self.search_within_results, "Search within results");
                }
                ui.add_space(16.0);

                ui.add_enabled_ui(!self.searching && !self.query.is_empty(), |ui| {
                    let label = if self.searching { "Searching…" } else { "Search" };
                    if ui
                        .add(
                            egui::Button::new(label)
                                .min_size(egui::vec2(ui.available_width(), 36.0)),
                        )
                        .clicked()
                    {
                        self.start_search(ctx);
                    }
                });

                if !self.results.is_empty() {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.label("Export Results:");
                    ui.horizontal(|ui| {
                        if ui.button("  JSON  ").clicked() {
                            self.export_json();
                        }
                        if ui.button("  CSV  ").clicked() {
                            self.export_csv();
                        }
                    });
                }
            });

        // ── Bottom status bar ────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                if self.searching {
                    ui.spinner();
                    ui.label("Searching…");
                } else if let Some(err) = &self.error_msg {
                    ui.colored_label(egui::Color32::from_rgb(230, 80, 80), err);
                } else {
                    ui.label(&self.status_msg);
                }
            });
            ui.add_space(5.0);
        });

        // ── Central panel ────────────────────────────────────────────────────
        let mut grid_loads: Vec<PathBuf> = Vec::new();
        let mut grid_vis: HashSet<PathBuf> = HashSet::new();
        let mut next_view: Option<ViewMode> = None;
        let mut copied_text: Option<String> = None;

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.view_mode {
                // ── Grid view ────────────────────────────────────────────────
                ViewMode::Grid => {
                    let n = self.results.len();
                    ui.heading(if n == 0 {
                        "Results".into()
                    } else {
                        format!("Results  ({n})")
                    });
                    ui.separator();

                    if self.results.is_empty() && !self.searching {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("No results to display.")
                                    .color(egui::Color32::GRAY)
                                    .size(15.0),
                            );
                        });
                        return;
                    }

                    let avail_w = ui.available_width();
                    let cell = GRID_THUMB + GRID_GAP;
                    let cols = ((avail_w + GRID_GAP) / cell).floor().max(1.0) as usize;
                    let total_rows = (self.results.len() + cols - 1) / cols;

                    egui::ScrollArea::vertical()
                        .id_salt("results_scroll")
                        .auto_shrink([false, false])
                        .show_rows(
                            ui,
                            GRID_THUMB + GRID_GAP,
                            total_rows,
                            |ui, row_range| {
                                for row in row_range {
                                    ui.horizontal(|ui| {
                                        for col in 0..cols {
                                            let idx = row * cols + col;
                                            if idx >= self.results.len() {
                                                break;
                                            }
                                            let path = &self.results[idx].record.path;
                                            let is_img = is_image(path);

                                            let (rect, resp) = ui.allocate_exact_size(
                                                egui::vec2(GRID_THUMB, GRID_THUMB),
                                                egui::Sense::click(),
                                            );

                                            if is_img {
                                                grid_vis.insert(path.clone());
                                            }

                                            let border = if resp.hovered() {
                                                egui::Stroke::new(
                                                    1.5,
                                                    egui::Color32::from_gray(200),
                                                )
                                            } else {
                                                egui::Stroke::new(
                                                    1.0,
                                                    egui::Color32::from_gray(55),
                                                )
                                            };

                                            let cell_bg = ui.visuals().panel_fill;
                                            let p = ui.painter();
                                            p.rect_filled(rect, 4.0, cell_bg);
                                            p.rect_stroke(rect, 4.0, border);

                                            match self.thumbnails.get(path) {
                                                Some(ThumbState::Loaded(tex)) => {
                                                    let ts = tex.size_vec2();
                                                    let s = (GRID_THUMB / ts.x)
                                                        .min(GRID_THUMB / ts.y);
                                                    let ds = ts * s;
                                                    let off = (egui::vec2(
                                                        GRID_THUMB, GRID_THUMB,
                                                    ) - ds)
                                                        * 0.5;
                                                    p.image(
                                                        tex.id(),
                                                        egui::Rect::from_min_size(
                                                            rect.min + off,
                                                            ds,
                                                        ),
                                                        egui::Rect::from_min_max(
                                                            egui::pos2(0.0, 0.0),
                                                            egui::pos2(1.0, 1.0),
                                                        ),
                                                        egui::Color32::WHITE,
                                                    );
                                                }
                                                Some(ThumbState::Failed) => {
                                                    p.text(
                                                        rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        "✕",
                                                        egui::FontId::proportional(26.0),
                                                        egui::Color32::DARK_RED,
                                                    );
                                                }
                                                None => {
                                                    if is_img {
                                                        grid_loads.push(path.clone());
                                                    } else {
                                                        p.text(
                                                            rect.center(),
                                                            egui::Align2::CENTER_CENTER,
                                                            "📄",
                                                            egui::FontId::proportional(32.0),
                                                            egui::Color32::GRAY,
                                                        );
                                                    }
                                                }
                                            }

                                            resp.clone().on_hover_text(
                                                path.file_name()
                                                    .unwrap_or_default()
                                                    .to_string_lossy()
                                                    .as_ref(),
                                            );

                                            if resp.clicked() {
                                                next_view = Some(ViewMode::Detail(idx));
                                            }

                                            ui.add_space(GRID_GAP);
                                        }
                                    });
                                    ui.add_space(GRID_GAP);
                                }
                            },
                        );
                }

                // ── Detail view ──────────────────────────────────────────────
                ViewMode::Detail(idx) => {
                    let result = &self.results[idx];
                    let path_str = result.record.path.display().to_string();
                    let filename = result
                        .record
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let generator = result.record.generator.to_string();
                    let score = result.score;
                    let prompt = result.record.prompt.clone();
                    let is_img = is_image(&result.record.path);
                    let thumb = match self.thumbnails.get(&result.record.path) {
                        Some(ThumbState::Loaded(h)) => Some(h.clone()),
                        _ => None,
                    };
                    let total = self.results.len();

                    // Keyboard navigation
                    ui.input(|i| {
                        if i.key_pressed(egui::Key::Escape) {
                            next_view = Some(ViewMode::Grid);
                        } else if i.key_pressed(egui::Key::ArrowRight) && idx + 1 < total {
                            next_view = Some(ViewMode::Detail(idx + 1));
                        } else if i.key_pressed(egui::Key::ArrowLeft) && idx > 0 {
                            next_view = Some(ViewMode::Detail(idx - 1));
                        }
                    });

                    // ── Top bar: Back / Prev / Next / position ───────────────
                    ui.horizontal(|ui| {
                        if ui.button("◀ Back").clicked() {
                            next_view = Some(ViewMode::Grid);
                        }
                        ui.add_space(4.0);
                        ui.add_enabled_ui(idx > 0, |ui| {
                            if ui.button("← Prev").clicked() {
                                next_view = Some(ViewMode::Detail(idx - 1));
                            }
                        });
                        ui.add_enabled_ui(idx + 1 < total, |ui| {
                            if ui.button("Next →").clicked() {
                                next_view = Some(ViewMode::Detail(idx + 1));
                            }
                        });
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(format!("{} / {total}", idx + 1))
                                .color(egui::Color32::GRAY),
                        );
                    });
                    ui.separator();
                    ui.add_space(4.0);

                    // ── Two-column layout: image left, info right ────────────
                    egui::ScrollArea::vertical()
                        .id_salt("detail_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                // Left: image
                                ui.vertical(|ui| {
                                    if let Some(tex) = &thumb {
                                        let ts = tex.size_vec2();
                                        let scale = (DETAIL_THUMB_MAX / ts.x)
                                            .min(DETAIL_THUMB_MAX / ts.y)
                                            .min(1.0);
                                        let ds = ts * scale;
                                        let (r, _) =
                                            ui.allocate_exact_size(ds, egui::Sense::hover());
                                        ui.painter().image(
                                            tex.id(),
                                            r,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            egui::Color32::WHITE,
                                        );
                                    } else if is_img {
                                        let ph = egui::vec2(DETAIL_THUMB_MAX, DETAIL_THUMB_MAX);
                                        let (r, _) =
                                            ui.allocate_exact_size(ph, egui::Sense::hover());
                                        ui.painter().rect_filled(
                                            r,
                                            6.0,
                                            egui::Color32::from_gray(30),
                                        );
                                        ui.painter().text(
                                            r.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "⏳",
                                            egui::FontId::proportional(40.0),
                                            egui::Color32::GRAY,
                                        );
                                    } else {
                                        let ph = egui::vec2(DETAIL_THUMB_MAX, DETAIL_THUMB_MAX);
                                        let (r, _) =
                                            ui.allocate_exact_size(ph, egui::Sense::hover());
                                        ui.painter().rect_filled(
                                            r,
                                            6.0,
                                            egui::Color32::from_gray(30),
                                        );
                                        ui.painter().text(
                                            r.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "📄",
                                            egui::FontId::proportional(60.0),
                                            egui::Color32::GRAY,
                                        );
                                    }
                                });

                                ui.add_space(12.0);
                                ui.separator();
                                ui.add_space(8.0);

                                // Right: metadata + prompt
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(&filename)
                                            .strong()
                                            .size(16.0),
                                    );
                                    ui.add_space(6.0);

                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&path_str)
                                                .color(egui::Color32::from_rgb(80, 190, 230))
                                                .small(),
                                        );
                                    });
                                    if ui
                                        .button("📋  Copy path")
                                        .on_hover_text("Copy full path to clipboard")
                                        .clicked()
                                    {
                                        copied_text = Some(path_str.clone());
                                    }

                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new(format!("Generator: {generator}"))
                                            .color(egui::Color32::from_gray(180)),
                                    );
                                    if let Some(s) = score {
                                        ui.label(
                                            egui::RichText::new(format!("Score: {s}"))
                                                .color(egui::Color32::from_rgb(130, 200, 130)),
                                        );
                                    }

                                    ui.add_space(10.0);
                                    ui.separator();
                                    ui.add_space(4.0);
                                    ui.label(egui::RichText::new("Prompt:").strong());
                                    ui.add_space(4.0);
                                    ui.label(&prompt);
                                });
                            });
                        });
                }
            }
        });

        // ── Apply grid-mode mutations ────────────────────────────────────────
        if matches!(self.view_mode, ViewMode::Grid) {
            self.thumbnails.retain(|p, state| {
                matches!(state, ThumbState::Failed) || grid_vis.contains(p)
            });
            self.thumb_queued.retain(|p| grid_vis.contains(p));
            for path in grid_loads {
                self.request_thumb(path, ctx);
            }
        } else if let ViewMode::Detail(idx) = self.view_mode {
            if let Some(result) = self.results.get(idx) {
                let path = result.record.path.clone();
                if is_image(&path) {
                    self.request_thumb(path, ctx);
                }
            }
        }

        if let Some(text) = copied_text {
            ctx.output_mut(|o| o.copied_text = text);
            self.status_msg = "Path copied to clipboard.".into();
        }
        if let Some(v) = next_view {
            self.view_mode = v;
        }
    }
}
