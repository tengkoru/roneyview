//! Antarmuka Roneyview (egui/eframe): navigasi, zoom, geser, rotasi, layar penuh.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
#[cfg(feature = "dialogs")]
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, Context, FontId, Key, Modifiers, PointerButton, Pos2,
    Rect, Sense, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use eframe::glow::HasContext;

use crate::loader::{self, AnimFrame, Command, Loaded, Outcome};
use crate::settings::{FitMode, Store};
use crate::source::Listing;

const BG: Color32 = Color32::from_gray(22);
/// Gambar yang disimpan di cache: sekian di belakang dan di depan gambar aktif.
const KEEP_BEHIND: usize = 2;
const KEEP_AHEAD: usize = 3;
const MIN_SCALE: f32 = 0.02;
const MAX_SCALE: f32 = 32.0;
const ZOOM_STEP: f32 = 1.25;
const PAN_STEP: f32 = 90.0;
/// Roda mouse: satu "notch" dihitung sebesar ini (poin).
const WHEEL_LINE_POINTS: f32 = 60.0;
const FLIP_THRESHOLD: f32 = 40.0;

/// Animasi (GIF/WebP/APNG): semua frame disimpan di sisi CPU dan diunggah
/// satu per satu ke SATU tekstur, jadi memori GPU tidak berlipat ganda.
struct Anim {
    frames: Vec<AnimFrame>,
    /// Indeks frame yang saat ini ada di tekstur.
    uploaded: AtomicUsize,
    truncated: bool,
}

struct Page {
    tex: TextureHandle,
    orig: [u32; 2],
    file_size: u64,
    bytes: usize,
    anim: Option<Anim>,
}

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Fit(FitMode),
    Custom(f32),
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Next,
    Prev,
    First,
    Last,
    OpenFile,
    OpenFolder,
    Fit(FitMode),
    ZoomIn,
    ZoomOut,
    RotateCw,
    RotateCcw,
    PanY(f32),
    ToggleFullscreen,
    ExitFullscreen,
    TogglePlay,
    ToggleBarLock,
    Help,
    Quit,
}

#[derive(Clone, Copy)]
enum DialogKind {
    File,
    Folder,
}

#[derive(Default)]
struct FrameInput {
    actions: Vec<Action>,
    dropped: Vec<PathBuf>,
    hovering_files: bool,
    wheel: f32,
    zoom: f32,
}

pub struct RoneyApp {
    store: Store,
    store_dirty: bool,
    last_save: Instant,

    listing: Option<Arc<Listing>>,
    session: u64,
    index: usize,
    dir: i8,

    cache: HashMap<usize, Arc<Page>>,
    /// Batas total ukuran tekstur di cache (byte). Halaman terjauh dibuang dulu.
    budget: usize,
    low_mem_notified: bool,

    /// Indeks yang sedang digeser di slider (belum dibuka); dibuka saat dilepas.
    scrub: Option<usize>,
    bar_locked: bool,
    /// Gambar sedang digeser dengan mouse: sembunyikan overlay navigasi.
    panning: bool,
    pending: HashMap<usize, bool>,
    errors: HashMap<usize, String>,
    shown: Option<(usize, Arc<Page>)>,
    placed_for: Option<(u64, usize)>,

    cmd_tx: Sender<Command>,
    out_rx: Receiver<Loaded>,

    #[cfg(feature = "dialogs")]
    dialog_tx: Sender<Option<PathBuf>>,
    #[cfg(feature = "dialogs")]
    dialog_rx: Receiver<Option<PathBuf>>,
    #[cfg(feature = "dialogs")]
    dialog_busy: bool,

    fit: FitMode,
    zoom: Zoom,
    rotation: u8,
    offset: Vec2,
    land_bottom: bool,
    view_rect: Rect,
    display_scale: f32,
    flip_accum: f32,
    flip_block_until: Instant,

    anim_key: Option<(u64, usize)>,
    anim_frame: usize,
    anim_next: Instant,
    anim_playing: bool,

    fullscreen: bool,
    show_help: bool,
    mipmaps: bool,
    notice: Option<(String, Instant)>,
    title: String,
}

/// Skala tampilan untuk satu mode muat. `img` dan `view` dalam poin.
fn fit_scale(mode: FitMode, img: Vec2, view: Vec2) -> f32 {
    let w = img.x.max(1.0);
    let h = img.y.max(1.0);
    let vw = view.x.max(1.0);
    let vh = view.y.max(1.0);
    match mode {
        FitMode::Fit => (vw / w).min(vh / h).min(1.0),
        FitMode::FitUpscale => (vw / w).min(vh / h),
        FitMode::Width => vw / w,
        FitMode::Height => vh / h,
        FitMode::Original => 1.0,
    }
}

fn clamp_offset(off: Vec2, size: Vec2, view: Vec2) -> Vec2 {
    let mx = ((size.x - view.x) / 2.0).max(0.0);
    let my = ((size.y - view.y) / 2.0).max(0.0);
    vec2(off.x.clamp(-mx, mx), off.y.clamp(-my, my))
}

struct Geo {
    scale: f32,
    size: Vec2,
    offset: Vec2,
}

impl RoneyApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        arg: Option<PathBuf>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let ctx = cc.egui_ctx.clone();
        ctx.set_theme(egui::Theme::Dark);
        ctx.style_mut(|s| {
            s.visuals.panel_fill = Color32::from_gray(30);
            s.visuals.window_corner_radius = 6.0.into();
        });

        // Mipmap membuat gambar besar yang dikecilkan tampak halus, tetapi
        // butuh OpenGL >= 3.0 (atau GLES >= 2.0); pada GPU lawas dimatikan.
        let mipmaps = cc.gl.as_ref().is_some_and(|gl| {
            let v = gl.version();
            v.major >= 3 || (v.is_embedded && v.major >= 2)
        });

        let (cmd_tx, out_rx) = loader::spawn(ctx.clone())?;
        #[cfg(feature = "dialogs")]
        let (dialog_tx, dialog_rx) = mpsc::channel();

        let store = Store::load();
        let fit = store.state.fit;
        let bar_locked = store.state.bar_locked;
        let last_origin = store.state.last_origin.clone();

        let mut app = RoneyApp {
            store,
            store_dirty: false,
            last_save: Instant::now(),
            listing: None,
            session: 0,
            index: 0,
            dir: 1,
            cache: HashMap::new(),
            budget: loader::cache_budget(),
            low_mem_notified: false,
            scrub: None,
            bar_locked,
            panning: false,
            pending: HashMap::new(),
            errors: HashMap::new(),
            shown: None,
            placed_for: None,
            cmd_tx,
            out_rx,
            #[cfg(feature = "dialogs")]
            dialog_tx,
            #[cfg(feature = "dialogs")]
            dialog_rx,
            #[cfg(feature = "dialogs")]
            dialog_busy: false,
            fit,
            zoom: Zoom::Fit(fit),
            rotation: 0,
            offset: Vec2::ZERO,
            land_bottom: false,
            view_rect: Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0)),
            display_scale: 1.0,
            flip_accum: 0.0,
            flip_block_until: Instant::now(),
            anim_key: None,
            anim_frame: 0,
            anim_next: Instant::now(),
            anim_playing: true,
            fullscreen: false,
            show_help: false,
            mipmaps,
            notice: None,
            title: String::new(),
        };

        match arg {
            Some(p) => app.open_path(&ctx, &p),
            None => {
                // Tanpa argumen: lanjutkan dari tempat terakhir.
                if let Some(o) = last_origin {
                    let p = PathBuf::from(o);
                    if p.exists() {
                        app.open_path(&ctx, &p);
                    }
                }
            }
        }
        Ok(app)
    }

    // ---------------------------------------------------------------- membuka

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now() + Duration::from_secs(4)));
    }

    fn open_path(&mut self, ctx: &Context, path: &Path) {
        let (listing, start) = match Listing::open(path) {
            Ok(v) => v,
            Err(e) => {
                self.notify(e);
                return;
            }
        };
        let index = match start {
            Some(i) => i,
            None => self.restore_index(&listing),
        }
        .min(listing.len().saturating_sub(1));

        self.session += 1;
        let listing = Arc::new(listing);
        let cmd = Command::Open {
            session: self.session,
            listing: listing.clone(),
        };
        if self.cmd_tx.send(cmd).is_err() {
            self.notify("Thread pemuat gambar berhenti; mulai ulang Roneyview.");
            return;
        }
        self.listing = Some(listing);
        self.index = index;
        self.dir = 1;
        self.cache.clear();
        self.pending.clear();
        self.errors.clear();
        self.shown = None;
        self.placed_for = None;
        self.rotation = 0;
        self.zoom = Zoom::Fit(self.fit);
        self.land_bottom = false;
        self.after_page_change(ctx);
    }

    fn restore_index(&self, listing: &Listing) -> usize {
        let Some(pos) = self.store.position_for(&listing.key) else {
            return 0;
        };
        // Cari berdasarkan nama dulu (tahan terhadap berkas yang bertambah/hilang).
        listing
            .names
            .iter()
            .position(|n| *n == pos.name)
            .unwrap_or(pos.index)
    }

    #[cfg(feature = "dialogs")]
    fn start_dialog(&mut self, ctx: &Context, kind: DialogKind) {
        if self.dialog_busy {
            return;
        }
        self.dialog_busy = true;
        let start_dir = self
            .listing
            .as_ref()
            .map(|l| {
                if l.is_archive() {
                    l.origin.parent().map(Path::to_path_buf).unwrap_or_default()
                } else {
                    l.origin.clone()
                }
            })
            .filter(|p| p.is_dir());
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("roneyview-dialog".into())
            .spawn(move || {
                let picked = std::panic::catch_unwind(|| {
                    let mut d = rfd::FileDialog::new();
                    if let Some(dir) = start_dir {
                        d = d.set_directory(dir);
                    }
                    match kind {
                        DialogKind::File => {
                            let mut exts: Vec<String> = Vec::new();
                            for e in crate::source::IMAGE_EXTS
                                .iter()
                                .chain(crate::source::ARCHIVE_EXTS.iter())
                            {
                                exts.push((*e).to_string());
                                exts.push(e.to_uppercase());
                            }
                            d.set_title("Buka gambar atau arsip")
                                .add_filter("Gambar dan arsip ZIP/CBZ", &exts)
                                .pick_file()
                        }
                        DialogKind::Folder => d.set_title("Buka folder gambar").pick_folder(),
                    }
                })
                .unwrap_or(None);
                let _ = tx.send(picked);
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.dialog_busy = false;
            self.notify("Tidak dapat membuka dialog berkas.");
        }
    }

    #[cfg(not(feature = "dialogs"))]
    fn start_dialog(&mut self, _ctx: &Context, _kind: DialogKind) {
        self.notify("Dialog berkas tidak tersedia di build ini. Seret berkas ke jendela.");
    }

    fn poll_dialog(&mut self, ctx: &Context) {
        #[cfg(feature = "dialogs")]
        while let Ok(result) = self.dialog_rx.try_recv() {
            self.dialog_busy = false;
            if let Some(p) = result {
                self.open_path(ctx, &p);
            }
        }
        #[cfg(not(feature = "dialogs"))]
        let _ = ctx;
    }

    // --------------------------------------------------------------- pemuatan

    fn max_side(ctx: &Context) -> usize {
        ctx.input(|i| i.max_texture_side)
    }

    fn after_page_change(&mut self, ctx: &Context) {
        let Some(l) = self.listing.clone() else {
            return;
        };
        let lo = self.index.saturating_sub(KEEP_BEHIND);
        let hi = self.index + KEEP_AHEAD;
        self.cache.retain(|i, _| (lo..=hi).contains(i));
        self.request_pages(ctx);
        if let Some(name) = l.names.get(self.index) {
            self.store.remember(&l.key, self.index, name);
            self.store_dirty = true;
        }
        self.update_title(ctx);
        ctx.request_repaint();
    }

    /// Buang halaman terjauh dari gambar aktif sampai total ukuran cache muat
    /// dalam anggaran. Gambar yang sedang dilihat tidak pernah dibuang.
    fn enforce_budget(&mut self) {
        loop {
            let total: usize = self.cache.values().map(|p| p.bytes).sum();
            if total <= self.budget {
                return;
            }
            let victim = self
                .cache
                .keys()
                .copied()
                .filter(|i| *i != self.index)
                .max_by_key(|i| i.abs_diff(self.index));
            match victim {
                Some(v) => {
                    self.cache.remove(&v);
                }
                None => return,
            }
        }
    }

    fn request_pages(&mut self, ctx: &Context) {
        let Some(l) = self.listing.clone() else {
            return;
        };
        let n = l.len() as isize;
        let max_side = Self::max_side(ctx);
        let cur = self.index as isize;
        let (a, b) = if self.dir >= 0 { (1, -1) } else { (-1, 1) };
        let mut wanted: Vec<(usize, bool)> = vec![(self.index, true)];

        // Pengaman: saat RAM sistem hampir habis, lepaskan cache dan matikan prefetch
        // supaya Roneyview tidak ikut menyeret sistem ke OOM / swap-thrash.
        let low_memory = loader::available_now().is_some_and(|b| b < loader::LOW_MEMORY);
        if low_memory {
            self.cache.retain(|i, _| *i == self.index);
            if !self.low_mem_notified {
                self.low_mem_notified = true;
                self.notify("Memori sistem hampir habis: prefetch dimatikan");
            }
        } else {
            self.low_mem_notified = false;
            for off in [a, 2 * a, b] {
                let i = cur + off;
                if i >= 0 && i < n {
                    wanted.push((i as usize, false));
                }
            }
        }
        for (i, primary) in wanted {
            if self.cache.contains_key(&i) || self.errors.contains_key(&i) {
                continue;
            }
            if let Some(&was_primary) = self.pending.get(&i) {
                if was_primary || !primary {
                    continue;
                }
            }
            self.pending.insert(i, primary);
            let cmd = Command::Load {
                session: self.session,
                index: i,
                primary,
                max_side,
            };
            if self.cmd_tx.send(cmd).is_err() {
                self.pending.remove(&i);
            }
        }
    }

    /// Animasi tanpa mipmap: membangun ulang mipmap di setiap frame terlalu mahal.
    fn texture_options(&self, animated: bool) -> TextureOptions {
        TextureOptions {
            magnification: egui::TextureFilter::Linear,
            minification: egui::TextureFilter::Linear,
            wrap_mode: egui::TextureWrapMode::ClampToEdge,
            mipmap_mode: (self.mipmaps && !animated).then_some(egui::TextureFilter::Linear),
        }
    }

    fn poll_loader(&mut self, ctx: &Context) {
        let mut got_current = false;
        while let Ok(msg) = self.out_rx.try_recv() {
            if msg.session != self.session {
                continue;
            }
            self.pending.remove(&msg.index);
            match msg.outcome {
                Outcome::Ready(d) => {
                    let lo = self.index.saturating_sub(KEEP_BEHIND);
                    let hi = self.index + KEEP_AHEAD;
                    if !(lo..=hi).contains(&msg.index) {
                        continue; // sudah terlalu jauh, buang
                    }
                    let px = d.pixels;
                    let animated = px.frames.is_some();
                    // Mipmap menambah ~1/3 memori GPU.
                    let mip_factor = |b: usize| if self.mipmaps && !animated { b * 4 / 3 } else { b };
                    let mut bytes = mip_factor(px.image.size[0] * px.image.size[1] * 4);
                    let anim = px.frames.map(|frames| {
                        // frames[0] berbagi data dengan `image`; hitung sisanya.
                        bytes = frames
                            .iter()
                            .map(|f| f.image.size[0] * f.image.size[1] * 4)
                            .sum();
                        Anim {
                            frames,
                            uploaded: AtomicUsize::new(0),
                            truncated: px.truncated,
                        }
                    });
                    if msg.index != self.index {
                        let held: usize = self.cache.values().map(|p| p.bytes).sum();
                        if held + bytes > self.budget {
                            continue; // prefetch tidak muat: jangan alokasikan tekstur GPU
                        }
                    }
                    let tex = ctx.load_texture(
                        format!("pg{}-{}", msg.session, msg.index),
                        px.image,
                        self.texture_options(animated),
                    );
                    let page = Arc::new(Page {
                        tex,
                        orig: px.orig,
                        file_size: d.file_size,
                        bytes,
                        anim,
                    });
                    got_current |= msg.index == self.index;
                    self.cache.insert(msg.index, page);
                    self.enforce_budget();
                }
                Outcome::Failed(e) => {
                    self.errors.insert(msg.index, e);
                    got_current |= msg.index == self.index;
                }
                Outcome::Skipped => {}
            }
        }
        if got_current {
            ctx.request_repaint();
        }
    }

    // -------------------------------------------------------------- navigasi

    fn count(&self) -> usize {
        self.listing.as_ref().map_or(0, |l| l.len())
    }

    fn step(&mut self, ctx: &Context, delta: isize, land_bottom: bool) {
        let n = self.count() as isize;
        if n == 0 {
            return;
        }
        let target = self.index as isize + delta;
        if target < 0 {
            self.notify("Ini gambar pertama");
        } else if target >= n {
            self.notify("Ini gambar terakhir");
        } else {
            self.go_to(ctx, target as usize, land_bottom);
        }
    }

    fn go_to(&mut self, ctx: &Context, target: usize, land_bottom: bool) {
        if target >= self.count() || target == self.index {
            return;
        }
        self.dir = if target > self.index { 1 } else { -1 };
        self.index = target;
        self.rotation = 0;
        self.zoom = Zoom::Fit(self.fit);
        self.land_bottom = land_bottom;
        self.after_page_change(ctx);
    }

    fn update_title(&mut self, ctx: &Context) {
        let Some(l) = &self.listing else {
            return;
        };
        let name = l.names.get(self.index).map_or("", String::as_str);
        let title = if l.is_archive() {
            let arc = l
                .origin
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{name} [{arc}] ({}/{}) - Roneyview", self.index + 1, l.len())
        } else {
            format!("{name} ({}/{}) - Roneyview", self.index + 1, l.len())
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    // ------------------------------------------------------------ geometri

    fn page_dims(&self, page: &Page, ppp: f32) -> Vec2 {
        let unit = 1.0 / ppp;
        let [w, h] = page.orig;
        let (w, h) = (w as f32 * unit, h as f32 * unit);
        if self.rotation % 2 == 1 {
            vec2(h, w)
        } else {
            vec2(w, h)
        }
    }

    fn geometry(&self, ctx: &Context, page: &Page, view: Rect) -> Geo {
        let dims = self.page_dims(page, ctx.pixels_per_point());
        let scale = match self.zoom {
            Zoom::Custom(z) => z,
            Zoom::Fit(m) => fit_scale(m, dims, view.size()),
        }
        .clamp(MIN_SCALE, MAX_SCALE);
        let size = dims * scale;
        Geo {
            scale,
            size,
            offset: clamp_offset(self.offset, size, view.size()),
        }
    }

    fn zoom_by(&mut self, ctx: &Context, factor: f32, anchor: Option<Pos2>) {
        let Some((_, page)) = self.shown.clone() else {
            return;
        };
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let view = self.view_rect;
        let geo = self.geometry(ctx, &page, view);
        let new_scale = (geo.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        if (new_scale - geo.scale).abs() < 1e-6 {
            return;
        }
        let anchor = anchor.unwrap_or_else(|| view.center());
        let center = view.center() + geo.offset;
        let new_center = anchor - (anchor - center) * (new_scale / geo.scale);
        self.offset = new_center - view.center();
        self.zoom = Zoom::Custom(new_scale);
    }

    fn current_page(&mut self) -> Option<Arc<Page>> {
        if let Some(p) = self.cache.get(&self.index) {
            self.shown = Some((self.index, p.clone()));
        } else if self.errors.contains_key(&self.index) {
            self.shown = None;
        }
        self.shown.as_ref().map(|(_, p)| p.clone())
    }

    // ------------------------------------------------------------- aksi

    fn perform(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::Next => self.step(ctx, 1, false),
            Action::Prev => self.step(ctx, -1, false),
            Action::First => self.go_to(ctx, 0, false),
            Action::Last => {
                let n = self.count();
                if n > 0 {
                    self.go_to(ctx, n - 1, false);
                }
            }
            Action::OpenFile => self.start_dialog(ctx, DialogKind::File),
            Action::OpenFolder => self.start_dialog(ctx, DialogKind::Folder),
            Action::Fit(m) => {
                self.fit = m;
                self.zoom = Zoom::Fit(m);
                self.placed_for = None;
                self.store.state.fit = m;
                self.store_dirty = true;
            }
            Action::ZoomIn => self.zoom_by(ctx, ZOOM_STEP, None),
            Action::ZoomOut => self.zoom_by(ctx, 1.0 / ZOOM_STEP, None),
            Action::RotateCw => self.rotate(1),
            Action::RotateCcw => self.rotate(3),
            Action::PanY(dy) => self.offset.y += dy,
            Action::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            Action::ExitFullscreen => {
                if self.fullscreen {
                    self.fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
            Action::TogglePlay => {
                self.anim_playing = !self.anim_playing;
                if self.anim_playing {
                    // Lanjutkan dengan jeda penuh frame yang sedang tampil.
                    self.anim_next = Instant::now();
                }
            }
            Action::ToggleBarLock => self.set_bar_lock(!self.bar_locked),
            Action::Help => self.show_help = !self.show_help,
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
        ctx.request_repaint();
    }

    fn rotate(&mut self, quarter_turns: u8) {
        self.rotation = (self.rotation + quarter_turns) % 4;
        // Pertahankan mode zoom, tetapi tempatkan ulang gambar.
        if let Zoom::Custom(_) = self.zoom {
            self.zoom = Zoom::Fit(self.fit);
        }
        self.placed_for = None;
    }

    fn read_input(&self, ctx: &Context) -> FrameInput {
        let fullscreen = self.fullscreen;
        ctx.input_mut(|i| {
            let mut out = FrameInput {
                zoom: 1.0,
                ..Default::default()
            };
            let none = Modifiers::NONE;
            let ctrl = Modifiers::CTRL;
            let shift = Modifiers::SHIFT;
            {
                let mut take = |m: Modifiers, k: Key, a: Action, i: &mut egui::InputState| {
                    if i.consume_key(m, k) {
                        out.actions.push(a);
                    }
                };
                take(ctrl | shift, Key::O, Action::OpenFolder, i);
                take(ctrl, Key::O, Action::OpenFile, i);
                take(ctrl, Key::Q, Action::Quit, i);
                take(shift, Key::R, Action::RotateCcw, i);
                take(none, Key::R, Action::RotateCw, i);
                take(shift, Key::Space, Action::Prev, i);
                take(none, Key::Space, Action::Next, i);
                take(none, Key::ArrowRight, Action::Next, i);
                take(none, Key::PageDown, Action::Next, i);
                take(none, Key::ArrowLeft, Action::Prev, i);
                take(none, Key::PageUp, Action::Prev, i);
                take(none, Key::Backspace, Action::Prev, i);
                take(none, Key::Home, Action::First, i);
                take(none, Key::End, Action::Last, i);
                take(none, Key::ArrowUp, Action::PanY(PAN_STEP), i);
                take(none, Key::ArrowDown, Action::PanY(-PAN_STEP), i);
                take(none, Key::Plus, Action::ZoomIn, i);
                take(none, Key::Equals, Action::ZoomIn, i);
                take(none, Key::Minus, Action::ZoomOut, i);
                take(none, Key::Num0, Action::Fit(FitMode::Original), i);
                take(none, Key::Num1, Action::Fit(FitMode::Original), i);
                take(shift, Key::F, Action::Fit(FitMode::FitUpscale), i);
                take(none, Key::F, Action::Fit(FitMode::Fit), i);
                take(none, Key::W, Action::Fit(FitMode::Width), i);
                take(none, Key::H, Action::Fit(FitMode::Height), i);
                take(none, Key::P, Action::TogglePlay, i);
                take(none, Key::L, Action::ToggleBarLock, i);
                take(none, Key::Enter, Action::ToggleFullscreen, i);
                take(none, Key::F11, Action::ToggleFullscreen, i);
                take(none, Key::F1, Action::Help, i);
                if fullscreen {
                    take(none, Key::Escape, Action::ExitFullscreen, i);
                }
            }

            for ev in &i.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                } = ev
                {
                    if modifiers.ctrl || modifiers.command {
                        continue; // ditangani sebagai zoom_delta
                    }
                    out.wheel += match unit {
                        egui::MouseWheelUnit::Point => delta.y,
                        egui::MouseWheelUnit::Line => delta.y * WHEEL_LINE_POINTS,
                        egui::MouseWheelUnit::Page => delta.y * 600.0,
                    };
                }
            }
            out.zoom = i.zoom_delta();
            out.hovering_files = !i.raw.hovered_files.is_empty();
            out.dropped = i
                .raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect();
            out
        })
    }

    fn wheel_flip(&mut self, ctx: &Context, delta: f32) {
        let now = Instant::now();
        if now < self.flip_block_until {
            self.flip_accum = 0.0;
            return;
        }
        if self.flip_accum != 0.0 && self.flip_accum.signum() != delta.signum() {
            self.flip_accum = 0.0;
        }
        self.flip_accum += delta;
        if self.flip_accum.abs() >= FLIP_THRESHOLD {
            let forward = self.flip_accum < 0.0;
            self.flip_accum = 0.0;
            self.flip_block_until = now + Duration::from_millis(220);
            if forward {
                self.step(ctx, 1, false);
            } else {
                self.step(ctx, -1, true);
            }
        }
    }

    // ------------------------------------------------------------ gambar

    fn menu_bar(&self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        egui::MenuBar::new().ui(ui, |ui| {
            let item = |ui: &mut egui::Ui, text: &str, short: &str, acts: &mut Vec<Action>, a: Action| {
                let mut b = egui::Button::new(text);
                if !short.is_empty() {
                    b = b.shortcut_text(short);
                }
                if ui.add(b).clicked() {
                    acts.push(a);
                }
            };
            ui.menu_button("Berkas", |ui| {
                item(ui, "Buka berkas atau arsip...", "Ctrl+O", acts, Action::OpenFile);
                item(ui, "Buka folder...", "Ctrl+Shift+O", acts, Action::OpenFolder);
                ui.separator();
                item(ui, "Keluar", "Ctrl+Q", acts, Action::Quit);
            });
            ui.menu_button("Tampilan", |ui| {
                let current = match self.zoom {
                    Zoom::Fit(m) => Some(m),
                    Zoom::Custom(_) => None,
                };
                for (m, label, short) in [
                    (FitMode::Fit, "Muat ke jendela (kecilkan saja)", "F"),
                    (FitMode::FitUpscale, "Muat ke jendela (perbesar juga)", "Shift+F"),
                    (FitMode::Width, "Sesuaikan lebar", "W"),
                    (FitMode::Height, "Sesuaikan tinggi", "H"),
                    (FitMode::Original, "Ukuran asli (100%)", "0"),
                ] {
                    let b = egui::Button::new(label)
                        .selected(current == Some(m))
                        .shortcut_text(short);
                    if ui.add(b).clicked() {
                        acts.push(Action::Fit(m));
                    }
                }
                ui.separator();
                item(ui, "Perbesar", "+", acts, Action::ZoomIn);
                item(ui, "Perkecil", "-", acts, Action::ZoomOut);
                ui.separator();
                item(ui, "Putar searah jarum jam", "R", acts, Action::RotateCw);
                item(ui, "Putar berlawanan jarum jam", "Shift+R", acts, Action::RotateCcw);
                ui.separator();
                item(ui, "Jeda / putar animasi", "P", acts, Action::TogglePlay);
                let lock = egui::Button::new("Kunci bar bawah")
                    .selected(self.bar_locked)
                    .shortcut_text("L");
                if ui.add(lock).clicked() {
                    acts.push(Action::ToggleBarLock);
                }
                ui.separator();
                item(ui, "Layar penuh", "Enter", acts, Action::ToggleFullscreen);
            });
            ui.menu_button("Navigasi", |ui| {
                item(ui, "Sebelumnya", "Kiri", acts, Action::Prev);
                item(ui, "Berikutnya", "Kanan", acts, Action::Next);
                item(ui, "Pertama", "Home", acts, Action::First);
                item(ui, "Terakhir", "End", acts, Action::Last);
            });
            ui.menu_button("Bantuan", |ui| {
                item(ui, "Pintasan keyboard", "F1", acts, Action::Help);
            });
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let Some(l) = &self.listing else {
                ui.label("Siap. Seret gambar, folder, atau ZIP ke jendela ini.");
                return;
            };
            let name = l.names.get(self.index).map_or("", String::as_str);
            ui.label(format!("{}/{}", self.index + 1, l.len()));
            ui.separator();
            if let Some((_, page)) = &self.shown {
                if self.cache.contains_key(&self.index) {
                    let [w, h] = page.orig;
                    ui.label(format!("{w} x {h} px"));
                    ui.separator();
                    ui.label(human_size(page.file_size));
                    ui.separator();
                    let scale = self.display_scale;
                    ui.label(format!("{:.0}%", scale * 100.0));
                    ui.separator();
                    if let Some(a) = &page.anim {
                        let state = if self.anim_playing { "" } else { ", jeda" };
                        let cut = if a.truncated { ", dipotong" } else { "" };
                        ui.label(format!(
                            "animasi {}/{}{state}{cut}",
                            self.anim_frame + 1,
                            a.frames.len()
                        ));
                        ui.separator();
                    }
                }
            }
            if self.errors.contains_key(&self.index) {
                ui.colored_label(Color32::from_rgb(235, 110, 110), "gagal dimuat");
                ui.separator();
            } else if !self.cache.contains_key(&self.index) {
                ui.label("memuat...");
                ui.separator();
            }
            ui.add(egui::Label::new(name).truncate());
        });
    }
}

impl RoneyApp {
    fn draw_view(&mut self, ctx: &Context, ui: &mut egui::Ui, inp: &FrameInput) {
        let rect = ui.available_rect_before_wrap();
        self.view_rect = rect;
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        self.panning = resp.dragged_by(PointerButton::Primary);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, BG);

        let Some(page) = self.current_page() else {
            self.draw_placeholder(&painter, rect);
            return;
        };
        let shown_index = self.shown.as_ref().map_or(0, |(i, _)| *i);
        let hover = ctx.pointer_hover_pos().filter(|p| rect.contains(*p));

        // Penempatan awal tiap kali gambar (atau rotasi/mode) berubah.
        let key = (self.session, shown_index);
        if self.placed_for != Some(key) {
            self.placed_for = Some(key);
            let geo = self.geometry(ctx, &page, rect);
            let max_y = ((geo.size.y - rect.height()) / 2.0).max(0.0);
            let y = if self.land_bottom {
                -max_y
            } else if matches!(self.zoom, Zoom::Fit(FitMode::Width)) {
                max_y
            } else {
                0.0
            };
            self.land_bottom = false;
            self.offset = vec2(0.0, y);
        }

        // Geser dengan seret kiri.
        if resp.dragged_by(PointerButton::Primary) {
            self.offset += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.perform(ctx, Action::ToggleFullscreen);
        }

        // Zoom (Ctrl+roda / cubit di touchpad), berjangkar di kursor.
        if (inp.zoom - 1.0).abs() > 1e-4 && hover.is_some() {
            self.zoom_by(ctx, inp.zoom, hover);
        }

        // Roda mouse: geser vertikal bila gambar melebihi jendela,
        // kalau sudah mentok (atau muat) pindah halaman.
        if inp.wheel != 0.0 && resp.hovered() {
            let geo = self.geometry(ctx, &page, rect);
            let max_y = ((geo.size.y - rect.height()) / 2.0).max(0.0);
            let overflow = max_y > 0.5;
            let at_limit = overflow
                && ((inp.wheel > 0.0 && geo.offset.y >= max_y - 0.5)
                    || (inp.wheel < 0.0 && geo.offset.y <= -max_y + 0.5));
            if overflow && !at_limit {
                self.offset = vec2(
                    geo.offset.x,
                    (geo.offset.y + inp.wheel).clamp(-max_y, max_y),
                );
                self.flip_accum = 0.0;
            } else {
                self.wheel_flip(ctx, inp.wheel);
            }
        }

        let geo = self.geometry(ctx, &page, rect);
        self.offset = geo.offset;
        self.display_scale = geo.scale;

        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let center = rect.center() + geo.offset;
        let mut img_rect = Rect::from_center_size(center, geo.size);
        if (geo.scale - 1.0).abs() < 1e-4 {
            // Ukuran asli: sejajarkan ke piksel fisik agar tajam.
            img_rect = Rect::from_min_size(pos2(snap(img_rect.min.x), snap(img_rect.min.y)), geo.size);
        }
        paint_page(&painter, page.tex.id(), img_rect, self.rotation);

        let can_pan = geo.size.x > rect.width() + 0.5 || geo.size.y > rect.height() + 0.5;
        if can_pan && resp.hovered() {
            ctx.set_cursor_icon(if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
    }

    fn draw_placeholder(&self, painter: &egui::Painter, rect: Rect) {
        let (text, color, size) = match (&self.listing, self.errors.get(&self.index)) {
            (None, _) => (
                "Roneyview\n\nSeret gambar, folder, atau ZIP/CBZ ke sini\natau tekan Ctrl+O. F1 untuk pintasan.".to_string(),
                Color32::from_gray(150),
                18.0,
            ),
            (Some(l), Some(err)) => {
                let name = l.names.get(self.index).map_or("", String::as_str);
                (
                    format!("Tidak dapat menampilkan\n{name}\n\n{err}"),
                    Color32::from_rgb(235, 130, 130),
                    16.0,
                )
            }
            (Some(_), None) => ("Memuat...".to_string(), Color32::from_gray(130), 16.0),
        };
        let galley = painter.layout(
            text,
            FontId::proportional(size),
            color,
            (rect.width() - 48.0).max(80.0),
        );
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, color);
    }

    fn draw_overlays(&mut self, ctx: &Context, hovering_files: bool) {
        if hovering_files {
            egui::Area::new(egui::Id::new("drop_hint"))
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.heading("Lepaskan untuk membuka");
                    });
                });
        }
        if let Some((text, until)) = &self.notice {
            let now = Instant::now();
            if now < *until {
                let text = text.clone();
                egui::Area::new(egui::Id::new("notice"))
                    .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -40.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(text);
                        });
                    });
                ctx.request_repaint_after(*until - now);
            } else {
                self.notice = None;
            }
        }
        if self.show_help {
            let mut open = true;
            egui::Window::new("Pintasan keyboard")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::Grid::new("help_grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                        for (k, d) in [
                            ("Kanan / PgDn / Spasi", "Gambar berikutnya"),
                            ("Kiri / PgUp / Backspace", "Gambar sebelumnya"),
                            ("Home / End", "Pertama / terakhir"),
                            ("Roda mouse", "Geser; pindah gambar bila sudah mentok"),
                            ("Atas / Bawah", "Geser vertikal"),
                            ("Seret kiri", "Geser gambar"),
                            ("Ctrl + roda / + / -", "Perbesar / perkecil"),
                            ("F / Shift+F", "Muat ke jendela (kecilkan / perbesar juga)"),
                            ("W / H", "Sesuaikan lebar / tinggi"),
                            ("0 atau 1", "Ukuran asli 100%"),
                            ("R / Shift+R", "Putar kanan / kiri"),
                            ("P", "Jeda / putar animasi (GIF, WebP, APNG)"),
                            ("L", "Kunci / lepas bar bawah"),
                            ("Mouse ke tepi kiri/kanan", "Tombol sebelumnya / berikutnya"),
                            ("Mouse ke bawah", "Bar: tombol, slider lompat, Kunci"),
                            ("Enter / F11 / klik ganda", "Layar penuh (Esc keluar)"),
                            ("Ctrl+O", "Buka berkas atau arsip"),
                            ("Ctrl+Shift+O", "Buka folder"),
                            ("Ctrl+Q", "Keluar"),
                        ] {
                            ui.monospace(k);
                            ui.label(d);
                            ui.end_row();
                        }
                    });
                });
            self.show_help = open;
        }
    }

    /// Majukan frame animasi bila waktunya. Tidak menjadwalkan repaint saat dijeda
    /// atau saat halaman bukan animasi, jadi CPU tetap idle untuk gambar statis.
    fn tick_animation(&mut self, ctx: &Context) {
        let Some(page) = self.current_page() else {
            return;
        };
        let shown_index = self.shown.as_ref().map_or(0, |(i, _)| *i);
        let Some(anim) = &page.anim else {
            self.anim_key = None;
            return;
        };
        let now = Instant::now();
        let key = Some((self.session, shown_index));
        let first_delay = anim.frames[0].delay;
        if self.anim_key != key {
            // Halaman animasi baru (atau kembali ke halaman lama): mulai dari frame 0.
            self.anim_key = key;
            self.anim_frame = 0;
            self.anim_next = now + first_delay;
            if anim.uploaded.load(Ordering::Relaxed) != 0 {
                self.upload_frame(&page, 0);
            }
        } else if self.anim_playing && now >= self.anim_next {
            self.anim_frame = (self.anim_frame + 1) % anim.frames.len();
            self.upload_frame(&page, self.anim_frame);
            let delay = anim.frames[self.anim_frame].delay;
            // Setelah stall panjang, jangan mengejar ketinggalan frame.
            self.anim_next = if now.saturating_duration_since(self.anim_next) > delay {
                now + delay
            } else {
                self.anim_next + delay
            };
        }
        if self.anim_playing {
            ctx.request_repaint_after(self.anim_next.saturating_duration_since(Instant::now()));
        }
    }

    fn upload_frame(&self, page: &Page, frame: usize) {
        let Some(anim) = &page.anim else {
            return;
        };
        let Some(f) = anim.frames.get(frame) else {
            return;
        };
        let mut tex = page.tex.clone();
        tex.set(f.image.clone(), self.texture_options(true));
        anim.uploaded.store(frame, Ordering::Relaxed);
    }

    fn save_state(&mut self) {
        if self.store_dirty {
            // Galat penyimpanan sengaja diabaikan: bukan alasan untuk mengganggu pengguna.
            let _ = self.store.save();
            self.store_dirty = false;
        }
        self.last_save = Instant::now();
    }
}

impl eframe::App for RoneyApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Sinkronkan status layar penuh bila diubah oleh window manager.
        if let Some(f) = ctx.input(|i| i.viewport().fullscreen) {
            self.fullscreen = f;
        }

        self.poll_loader(ctx);
        self.poll_dialog(ctx);

        let inp = self.read_input(ctx);
        if let Some(p) = inp.dropped.first() {
            self.open_path(ctx, p);
        }
        for a in &inp.actions {
            self.perform(ctx, *a);
        }

        self.tick_animation(ctx);

        let mut acts: Vec<Action> = Vec::new();
        if !self.fullscreen {
            egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui, &mut acts));
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.status_bar(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ctx, |ui| self.draw_view(ctx, ui, &inp));
        self.draw_nav_overlays(ctx);
        for a in acts {
            self.perform(ctx, a);
        }

        self.draw_overlays(ctx, inp.hovering_files);

        if self.store_dirty {
            if self.last_save.elapsed() >= Duration::from_millis(1500) {
                self.save_state();
            } else {
                ctx.request_repaint_after(Duration::from_millis(1600));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_state();
    }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.2} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn paint_page(painter: &egui::Painter, tex: egui::TextureId, rect: Rect, rotation: u8) {
    let uv = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let r = (rotation % 4) as usize;
    let mut mesh = egui::Mesh::with_texture(tex);
    for (k, pos) in corners.iter().enumerate() {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *pos,
            uv: uv[(k + 4 - r) % 4],
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

// ------------------------------------------------------- overlay navigasi

/// Tinggi zona bawah yang memunculkan bar navigasi (poin).
const BAR_ZONE_H: f32 = 110.0;
const BAR_GAP: f32 = 14.0;
const FADE_SECS: f32 = 0.18;
const ACCENT: Color32 = Color32::from_rgb(90, 160, 255);

fn side_zone_width(view_w: f32) -> f32 {
    (view_w * 0.15).clamp(70.0, 170.0)
}

/// (zona kiri, zona kanan, zona bawah) untuk area gambar `view`.
fn nav_zones(view: Rect) -> (Rect, Rect, Rect) {
    let w = side_zone_width(view.width());
    let left = Rect::from_min_max(view.min, pos2(view.min.x + w, view.max.y));
    let right = Rect::from_min_max(pos2(view.max.x - w, view.min.y), view.max);
    let top = (view.max.y - BAR_ZONE_H).max(view.min.y);
    let bottom = Rect::from_min_max(pos2(view.min.x, top), view.max);
    (left, right, bottom)
}

/// Posisi x di slider -> indeks gambar (0-based), dijepit ke ujung.
fn scrub_index(x: f32, left: f32, width: f32, n: usize) -> usize {
    if n <= 1 || width <= 0.0 || !x.is_finite() {
        return 0;
    }
    let t = ((x - left) / width).clamp(0.0, 1.0);
    ((t * (n - 1) as f32).round() as usize).min(n - 1)
}

/// Kebalikan `scrub_index`: posisi x pegangan slider untuk indeks tertentu.
fn scrub_x(idx: usize, left: f32, width: f32, n: usize) -> f32 {
    if n <= 1 {
        return left;
    }
    left + width * (idx.min(n - 1) as f32 / (n - 1) as f32)
}

/// Pendekkan nama panjang di tengah ("foto_aaaa...zzzz.jpg"), hitung per karakter.
fn truncate_middle(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max || max < 8 {
        return name.to_string();
    }
    let head = (max - 3) / 2 + (max - 3) % 2;
    let tail = (max - 3) / 2;
    let mut out: String = chars[..head].iter().collect();
    out.push_str("...");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

#[derive(Clone, Copy)]
enum Chevron {
    Left,
    Right,
}

fn paint_chevron(painter: &egui::Painter, rect: Rect, dir: Chevron, color: Color32, width: f32) {
    let c = rect.center();
    let h = rect.height().min(rect.width() * 1.6) * 0.26;
    let w = h * 0.55;
    let (a, b, d) = match dir {
        Chevron::Left => (pos2(c.x + w, c.y - h), pos2(c.x - w, c.y), pos2(c.x + w, c.y + h)),
        Chevron::Right => (pos2(c.x - w, c.y - h), pos2(c.x + w, c.y), pos2(c.x - w, c.y + h)),
    };
    painter.add(egui::Shape::line(vec![a, b, d], egui::Stroke::new(width, color)));
}

/// Tombol dengan panah yang digambar sendiri (tanpa bergantung pada glyph font).
fn icon_button(
    ui: &mut egui::Ui,
    size: Vec2,
    dir: Chevron,
    enabled: bool,
    round: f32,
    base_alpha: u8,
) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let alpha = if pressed {
        225
    } else if hovered {
        200
    } else {
        base_alpha
    };
    let painter = ui.painter();
    painter.rect_filled(rect, round, Color32::from_black_alpha(alpha));
    let col = if enabled {
        Color32::WHITE
    } else {
        Color32::from_white_alpha(70)
    };
    paint_chevron(painter, rect, dir, col, 2.6);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

struct BarInput {
    n: usize,
    index: usize,
    scrub: Option<usize>,
    locked: bool,
    /// Lebar isi (di dalam margin bingkai).
    width: f32,
}

#[derive(Default)]
struct BarOutput {
    prev: bool,
    next: bool,
    locked: bool,
    /// Indeks di bawah pointer selama tombol mouse ditahan pada slider.
    scrub: Option<usize>,
    /// Tombol mouse sedang ditahan pada slider.
    active: bool,
    slider: Option<Rect>,
}

fn draw_bar(ui: &mut egui::Ui, inp: &BarInput) -> BarOutput {
    let mut out = BarOutput {
        locked: inp.locked,
        ..Default::default()
    };
    let last = inp.n.saturating_sub(1);
    let btn = vec2(32.0, 28.0);
    let label_w = 84.0;
    let lock_w = 78.0;
    let gap = 8.0;
    let slider_w = (inp.width - 2.0 * btn.x - label_w - lock_w - 4.0 * gap).max(60.0);

    ui.spacing_mut().item_spacing.x = gap;
    ui.horizontal(|ui| {
        out.prev = icon_button(ui, btn, Chevron::Left, inp.index > 0, 6.0, 90).clicked();

        // Slider kustom: nilai mengikuti pointer selama tombol ditahan.
        let sense = if inp.n > 1 {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(slider_w, btn.y), sense);
        let pad = 9.0;
        let track_left = rect.left() + pad;
        let track_w = (rect.width() - 2.0 * pad).max(1.0);
        let down = inp.n > 1 && resp.is_pointer_button_down_on();
        if down {
            if let Some(p) = resp.interact_pointer_pos() {
                out.scrub = Some(scrub_index(p.x, track_left, track_w, inp.n));
            }
            out.active = true;
        }
        let shown = out
            .scrub
            .or(inp.scrub)
            .unwrap_or(inp.index)
            .min(last);
        let hx = scrub_x(shown, track_left, track_w, inp.n);
        let cy = rect.center().y;
        let painter = ui.painter();
        let track = Rect::from_min_max(pos2(track_left, cy - 2.0), pos2(track_left + track_w, cy + 2.0));
        painter.rect_filled(track, 2.0, Color32::from_white_alpha(55));
        painter.rect_filled(
            Rect::from_min_max(track.min, pos2(hx, track.max.y)),
            2.0,
            ACCENT,
        );
        let r = if down {
            9.0
        } else if resp.hovered() {
            8.0
        } else {
            7.0
        };
        painter.circle_filled(pos2(hx, cy), r, Color32::WHITE);
        if inp.n > 1 {
            let _ = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        out.slider = Some(rect);

        out.next = icon_button(ui, btn, Chevron::Right, inp.index < last, 6.0, 90).clicked();

        ui.add_sized(
            [label_w, btn.y],
            egui::Label::new(
                egui::RichText::new(format!("{}/{}", shown + 1, inp.n))
                    .monospace()
                    .color(Color32::WHITE),
            ),
        );

        let mut locked = inp.locked;
        let cb = ui
            .add_sized([lock_w, btn.y], egui::Checkbox::new(&mut locked, "Kunci"))
            .on_hover_text("Kunci: bar ini tetap tampil walau pointer menjauh (L)");
        if cb.changed() {
            out.locked = locked;
        }
    });
    out
}

impl RoneyApp {
    fn set_bar_lock(&mut self, locked: bool) {
        self.bar_locked = locked;
        self.store.state.bar_locked = locked;
        self.store_dirty = true;
    }

    /// Tombol prev/next di tepi kiri-kanan dan bar bawah (tombol + slider + Kunci).
    /// Semuanya muncul perlahan hanya saat pointer berada di zonanya.
    fn draw_nav_overlays(&mut self, ctx: &Context) {
        let n = self.count();
        if n == 0 {
            self.scrub = None;
            return;
        }
        let view = self.view_rect;
        let (zone_l, zone_r, zone_b) = nav_zones(view);
        let ptr = ctx.pointer_hover_pos();
        let busy = self.panning;
        let over = |z: Rect| !busy && ptr.is_some_and(|p| z.contains(p));

        let t_left = ctx.animate_bool_with_time(
            egui::Id::new("nav_left"),
            self.index > 0 && over(zone_l),
            FADE_SECS,
        );
        let t_right = ctx.animate_bool_with_time(
            egui::Id::new("nav_right"),
            self.index + 1 < n && over(zone_r),
            FADE_SECS,
        );
        // Bar tetap tampil selama dikunci atau slider sedang ditahan.
        let want_bar = self.bar_locked || self.scrub.is_some() || over(zone_b);
        let t_bar = ctx.animate_bool_with_time(egui::Id::new("nav_bar"), want_bar, FADE_SECS);

        let mut action: Option<Action> = None;
        let side_size = vec2(46.0, 88.0);
        if t_left > 0.02 {
            let clicked = egui::Area::new(egui::Id::new("nav_left_btn"))
                .order(egui::Order::Foreground)
                .pivot(Align2::LEFT_CENTER)
                .fixed_pos(pos2(view.min.x + 14.0, view.center().y))
                .show(ctx, |ui| {
                    ui.set_opacity(t_left);
                    icon_button(ui, side_size, Chevron::Left, true, 12.0, 120).clicked()
                })
                .inner;
            if clicked {
                action = Some(Action::Prev);
            }
        }
        if t_right > 0.02 {
            let clicked = egui::Area::new(egui::Id::new("nav_right_btn"))
                .order(egui::Order::Foreground)
                .pivot(Align2::RIGHT_CENTER)
                .fixed_pos(pos2(view.max.x - 14.0, view.center().y))
                .show(ctx, |ui| {
                    ui.set_opacity(t_right);
                    icon_button(ui, side_size, Chevron::Right, true, 12.0, 120).clicked()
                })
                .inner;
            if clicked {
                action = Some(Action::Next);
            }
        }

        let mut bar: Option<BarOutput> = None;
        if t_bar > 0.02 {
            let bar_w = (view.width() - 24.0).clamp(220.0, 780.0);
            let input = BarInput {
                n,
                index: self.index,
                scrub: self.scrub,
                locked: self.bar_locked,
                width: bar_w - 20.0,
            };
            let out = egui::Area::new(egui::Id::new("nav_bar_area"))
                .order(egui::Order::Foreground)
                .pivot(Align2::CENTER_BOTTOM)
                .fixed_pos(pos2(view.center().x, view.max.y - BAR_GAP))
                .show(ctx, |ui| {
                    ui.set_opacity(t_bar);
                    egui::Frame::new()
                        .fill(Color32::from_black_alpha(205))
                        .corner_radius(10.0)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| draw_bar(ui, &input))
                        .inner
                })
                .inner;
            bar = Some(out);
        }

        // Slider: halaman baru dibuka saat tombol mouse dilepas (bukan di tiap piksel
        // geseran), supaya menyeret melewati ratusan foto besar tidak memuat semuanya.
        let active = bar.as_ref().is_some_and(|b| b.active);
        if active {
            if let Some(b) = &bar {
                self.scrub = b.scrub.or(self.scrub);
            }
        } else if let Some(target) = self.scrub.take() {
            self.go_to(ctx, target, false);
        }

        // Petunjuk melayang di atas pegangan slider saat digeser.
        if let (Some(sc), Some(sl)) = (self.scrub, bar.as_ref().and_then(|b| b.slider)) {
            let hx = scrub_x(sc, sl.left() + 9.0, (sl.width() - 18.0).max(1.0), n);
            let name = self
                .listing
                .as_ref()
                .and_then(|l| l.names.get(sc))
                .map_or(String::new(), |s| truncate_middle(s, 44));
            egui::Area::new(egui::Id::new("nav_scrub_tip"))
                .order(egui::Order::Foreground)
                .pivot(Align2::CENTER_BOTTOM)
                .fixed_pos(pos2(
                    hx.clamp(view.left() + 100.0, view.right() - 100.0),
                    sl.top() - 12.0,
                ))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(format!("{}/{}  {}", sc + 1, n, name));
                    });
                });
        }

        if let Some(b) = &bar {
            if b.prev {
                action = Some(Action::Prev);
            } else if b.next {
                action = Some(Action::Next);
            }
            if b.locked != self.bar_locked {
                self.set_bar_lock(b.locked);
            }
        }
        if let Some(a) = action {
            self.perform(ctx, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_muat_menghitung_skala_dengan_benar() {
        let img = vec2(2000.0, 1000.0);
        let view = vec2(1000.0, 800.0);
        assert!((fit_scale(FitMode::Fit, img, view) - 0.5).abs() < 1e-6);
        assert!((fit_scale(FitMode::Width, img, view) - 0.5).abs() < 1e-6);
        assert!((fit_scale(FitMode::Height, img, view) - 0.8).abs() < 1e-6);
        assert_eq!(fit_scale(FitMode::Original, img, view), 1.0);
        // Gambar kecil: Fit tidak memperbesar, FitUpscale memperbesar.
        let small = vec2(100.0, 50.0);
        assert_eq!(fit_scale(FitMode::Fit, small, view), 1.0);
        assert!((fit_scale(FitMode::FitUpscale, small, view) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn ukuran_nol_tidak_menyebabkan_pembagian_nol() {
        assert!(fit_scale(FitMode::Fit, vec2(0.0, 0.0), vec2(0.0, 0.0)).is_finite());
    }

    #[test]
    fn offset_dijepit_dan_gambar_kecil_dipusatkan() {
        let view = vec2(100.0, 100.0);
        assert_eq!(clamp_offset(vec2(50.0, 50.0), vec2(80.0, 80.0), view), Vec2::ZERO);
        assert_eq!(
            clamp_offset(vec2(500.0, -500.0), vec2(300.0, 200.0), view),
            vec2(100.0, -50.0)
        );
    }

    #[test]
    fn ukuran_berkas_terbaca_manusiawi() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.00 MB");
    }

    #[test]
    fn zona_navigasi_kiri_kanan_bawah() {
        let view = Rect::from_min_size(pos2(0.0, 24.0), vec2(1000.0, 700.0));
        let (l, r, b) = nav_zones(view);
        assert!(l.contains(pos2(10.0, 300.0)));
        assert!(!l.contains(pos2(500.0, 300.0)));
        assert!(r.contains(pos2(990.0, 300.0)));
        assert!(!r.contains(pos2(500.0, 300.0)));
        assert!(b.contains(pos2(500.0, 720.0)));
        assert!(!b.contains(pos2(500.0, 300.0)));
        assert_eq!(side_zone_width(200.0), 70.0);
        assert_eq!(side_zone_width(1000.0), 150.0);
        assert_eq!(side_zone_width(5000.0), 170.0);
        // jendela sangat pendek: zona bawah tidak keluar dari area
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 60.0));
        let (_, _, b) = nav_zones(tiny);
        assert!(b.min.y >= tiny.min.y);
    }

    #[test]
    fn slider_menjangkau_setiap_gambar_tepat_dan_dijepit_di_ujung() {
        for n in [2usize, 3, 46, 100, 1000] {
            for idx in 0..n {
                let x = scrub_x(idx, 10.0, 300.0, n);
                assert_eq!(scrub_index(x, 10.0, 300.0, n), idx, "n={n} idx={idx}");
            }
        }
        // contoh pengguna: gambar ke-46 dari 100 = indeks 45
        let x = scrub_x(45, 0.0, 400.0, 100);
        assert_eq!(scrub_index(x, 0.0, 400.0, 100) + 1, 46);
        // di luar slider: dijepit
        assert_eq!(scrub_index(-500.0, 10.0, 300.0, 100), 0);
        assert_eq!(scrub_index(9999.0, 10.0, 300.0, 100), 99);
        // kasus tepi tidak panik
        assert_eq!(scrub_index(5.0, 0.0, 100.0, 0), 0);
        assert_eq!(scrub_index(5.0, 0.0, 100.0, 1), 0);
        assert_eq!(scrub_index(f32::NAN, 0.0, 100.0, 10), 0);
        assert_eq!(scrub_index(5.0, 0.0, 0.0, 10), 0);
        assert_eq!(scrub_x(0, 7.0, 100.0, 1), 7.0);
    }

    #[test]
    fn nama_panjang_dipendekkan_di_tengah_tanpa_memotong_karakter() {
        assert_eq!(truncate_middle("pendek.jpg", 44), "pendek.jpg");
        let long = "foto_liburan_keluarga_besar_2026_bagian_sangat_panjang_sekali.jpg";
        let t = truncate_middle(long, 30);
        assert_eq!(t.chars().count(), 30);
        assert!(t.starts_with("foto_liburan"));
        assert!(t.ends_with(".jpg"));
        assert!(t.contains("..."));
        // karakter multibyte tidak boleh membuat panik
        let uni = "日本語のとても長いファイル名その二その三その四.png";
        let t = truncate_middle(uni, 12);
        assert_eq!(t.chars().count(), 12);
    }
}
