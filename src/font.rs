//! Monospace font loading, glyph rasterization and atlas packing.
//!
//! Glyphs are rasterized on demand into a single coverage (R8) texture. The atlas is never
//! shaped: the grid is monospace, so each `char` maps to exactly one glyph and one cell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::trace;

/// Candidate fonts, in preference order. SF Mono ships with Terminal.app on every macOS;
/// Menlo is the fallback (and has far wider Unicode coverage, so it is also the fallback face).
const DEFAULT_FONTS: &[(&str, u32)] = &[
    (
        "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SFMono-Terminal.ttf",
        0,
    ),
    ("/System/Library/Fonts/SFNSMono.ttf", 0),
    ("/System/Library/Fonts/Menlo.ttc", 0),
];

const FALLBACK_FONTS: &[(&str, u32)] = &[("/System/Library/Fonts/Menlo.ttc", 0)];

#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    /// Atlas texture coordinates (u0, v0, u1, v1), normalized.
    pub uv: [f32; 4],
    /// Bitmap size in pixels.
    pub size: [f32; 2],
    /// Offset of the bitmap's top-left corner from the cell's top-left corner, in pixels.
    pub offset: [f32; 2],
    /// Number of cells the glyph occupies (1, or 2 for East Asian wide characters).
    pub cells: u8,
}

struct Face {
    font: fontdue::Font,
}

pub struct FontSet {
    primary: Face,
    fallback_paths: Vec<(PathBuf, u32)>,
    fallbacks: Vec<Face>,
    fallbacks_loaded: bool,
}

impl FontSet {
    /// Loads the first usable font from `explicit` (a file path or a family name, see
    /// [`resolve`]) or the default candidates. A configured font that cannot be used is
    /// reported on stderr and the defaults are tried.
    pub fn load(explicit: Option<&str>, px: f32) -> Result<FontSet, String> {
        let _s = trace::span("font-load");
        let mut candidates: Vec<(PathBuf, u32)> = Vec::new();
        if let Some(spec) = explicit {
            match resolve(spec) {
                Some(p) => candidates.push((p, 0)),
                None => eprintln!("diffvader: font {spec:?} not found; using the default font"),
            }
        }
        candidates.extend(DEFAULT_FONTS.iter().map(|(p, i)| (PathBuf::from(p), *i)));
        let mut last_err = String::from("no font candidates");
        for (path, index) in &candidates {
            match load_face(path, *index, px) {
                Ok(font) => {
                    if explicit.is_some() && candidates.first().is_some_and(|c| c.0 != *path) {
                        eprintln!(
                            "diffvader: cannot use font {:?}; using {}",
                            explicit.unwrap_or(""),
                            path.display()
                        );
                    }
                    return Ok(FontSet {
                        primary: Face { font },
                        fallback_paths: FALLBACK_FONTS
                            .iter()
                            .map(|(p, i)| (PathBuf::from(p), *i))
                            .filter(|(p, _)| p != path)
                            .collect(),
                        fallbacks: Vec::new(),
                        fallbacks_loaded: false,
                    });
                }
                Err(e) => last_err = format!("{}: {}", path.display(), e),
            }
        }
        Err(last_err)
    }

    /// Cell width and line height in pixels at `px`, both rounded to whole pixels so that the
    /// grid stays crisp.
    pub fn metrics(&self, px: f32) -> (f32, f32, f32) {
        let font = &self.primary.font;
        let cell_w = font.metrics('M', px).advance_width.round().max(1.0);
        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal metrics");
        let line_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0);
        let ascent = lm.ascent.round();
        (cell_w, line_h, ascent)
    }

    fn face_for(&mut self, c: char) -> Option<&Face> {
        if self.primary.font.has_glyph(c) {
            return Some(&self.primary);
        }
        if !self.fallbacks_loaded {
            self.fallbacks_loaded = true;
            let _s = trace::span("font-load-fallback");
            let paths = std::mem::take(&mut self.fallback_paths);
            for (p, i) in paths {
                if let Ok(font) = load_face(&p, i, 40.0) {
                    self.fallbacks.push(Face { font });
                }
            }
        }
        self.fallbacks.iter().find(|f| f.font.has_glyph(c))
    }
}

const FONT_DIRS: &[&str] = &[
    "~/Library/Fonts",
    "/Library/Fonts",
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts",
];

/// A font file for `spec`: the path itself when it exists, else the font file in the usual
/// directories whose name best matches the family name. "JetBrains Mono" matches
/// `JetBrainsMono-Regular.ttf`; a regular / book weight is preferred, then the shortest
/// name, so "Fira Code" picks `FiraCode-Regular.ttf` over `FiraCode-Bold.ttf`.
pub fn resolve(spec: &str) -> Option<PathBuf> {
    let as_path = Path::new(spec);
    if as_path.is_file() {
        return Some(as_path.to_path_buf());
    }
    let _s = trace::span("font-resolve");
    let want = normalize(spec);
    if want.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut best: Option<(u8, usize, PathBuf)> = None;
    for dir in FONT_DIRS {
        let dir = match dir.strip_prefix("~/") {
            Some(rest) => match &home {
                Some(h) => h.join(rest),
                None => continue,
            },
            None => PathBuf::from(dir),
        };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let stem = normalize(stem);
            let Some(rest) = stem.strip_prefix(want.as_str()) else {
                continue;
            };
            let rank = match rest {
                "" | "regular" | "book" | "roman" => 0,
                "medium" | "text" => 1,
                r if r.contains("italic") || r.contains("oblique") => 3,
                _ => 2,
            };
            let key = (rank, rest.len());
            if best.as_ref().is_none_or(|(r, l, _)| key < (*r, *l)) {
                best = Some((rank, rest.len(), path));
            }
        }
    }
    best.map(|(_, _, p)| p)
}

/// Lowercase alphanumerics only, so "SF Mono", "SFMono" and "sf-mono" compare equal.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn load_face(path: &Path, index: u32, px: f32) -> Result<fontdue::Font, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    fontdue::Font::from_bytes(
        data,
        fontdue::FontSettings {
            collection_index: index,
            scale: px,
            // Substitution glyphs are only reachable via shaping, which we never do.
            load_substitutions: false,
        },
    )
    .map_err(|e| e.to_string())
}

/// Coverage texture with shelf packing. All glyphs of one atlas share the pixel size, so
/// shelves have a fixed height and packing is trivial.
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Region that changed since the last upload (x0, y0, x1, y1), empty if x0 >= x1.
    pub dirty: [u32; 4],
    shelf_h: u32,
    cursor_x: u32,
    cursor_y: u32,
    glyphs: HashMap<char, Option<Glyph>>,
    px: f32,
    ascent: f32,
    cell_w: f32,
    /// Set when the atlas ran out of room and was cleared; the current frame's glyph
    /// references are stale and the caller must rebuild its draw list.
    pub reset: bool,
}

impl Atlas {
    pub fn new(px: f32, cell_w: f32, line_h: f32, ascent: f32) -> Atlas {
        let size = 1024u32;
        let mut a = Atlas {
            width: size,
            height: size,
            pixels: vec![0; (size * size) as usize],
            dirty: [0, 0, 0, 0],
            shelf_h: line_h as u32 + 2,
            cursor_x: 0,
            cursor_y: 0,
            glyphs: HashMap::new(),
            px,
            ascent,
            cell_w,
            reset: false,
        };
        a.reserve_white();
        a
    }

    /// A solid 2x2 white block at (0,0) so that solid rectangles can go through the same
    /// pipeline as glyphs (they sample this block).
    fn reserve_white(&mut self) {
        for y in 0..2 {
            for x in 0..2 {
                self.pixels[(y * self.width + x) as usize] = 255;
            }
        }
        self.cursor_x = 4;
        self.mark_dirty(0, 0, 4, 2);
    }

    pub fn white_uv(&self) -> [f32; 4] {
        let u = 1.0 / self.width as f32;
        let v = 1.0 / self.height as f32;
        [u * 0.5, v * 0.5, u * 1.5, v * 1.5]
    }

    fn mark_dirty(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        if self.dirty[0] >= self.dirty[2] {
            self.dirty = [x0, y0, x1, y1];
        } else {
            self.dirty[0] = self.dirty[0].min(x0);
            self.dirty[1] = self.dirty[1].min(y0);
            self.dirty[2] = self.dirty[2].max(x1);
            self.dirty[3] = self.dirty[3].max(y1);
        }
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = [0, 0, 0, 0];
    }

    /// Returns the glyph for `c`, rasterizing it on first use. `None` means the character
    /// is blank or no font can draw it.
    pub fn glyph(&mut self, fonts: &mut FontSet, c: char) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&c) {
            return *g;
        }
        let g = self.rasterize(fonts, c);
        self.glyphs.insert(c, g);
        g
    }

    fn rasterize(&mut self, fonts: &mut FontSet, c: char) -> Option<Glyph> {
        let _s = trace::span("glyph-rasterize");
        let face = fonts.face_for(c)?;
        let cells = unicode_width::UnicodeWidthChar::width(c)
            .unwrap_or(1)
            .clamp(1, 2) as u8;
        let (metrics, mut bitmap) = face.font.rasterize(c, self.px);
        if metrics.width == 0 || metrics.height == 0 {
            return None;
        }
        // fontdue's linear coverage reads thin next to macOS's text; a mild gamma lift
        // brings stem weight closer to what Terminal.app shows.
        for v in bitmap.iter_mut() {
            *v = GAMMA_LUT[*v as usize];
        }
        let w = metrics.width as u32;
        let h = metrics.height as u32;
        let pad = 1;
        if w + 2 * pad > self.width || h + 2 * pad > self.shelf_h.max(h + 2 * pad) {
            return None;
        }
        if self.cursor_x + w + 2 * pad > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.shelf_h;
        }
        let shelf_h = self.shelf_h.max(h + 2 * pad);
        if self.cursor_y + shelf_h > self.height {
            self.reset_atlas();
        }
        let x0 = self.cursor_x + pad;
        let y0 = self.cursor_y + pad;
        for row in 0..h {
            let dst = ((y0 + row) * self.width + x0) as usize;
            let src = (row * w) as usize;
            self.pixels[dst..dst + w as usize].copy_from_slice(&bitmap[src..src + w as usize]);
        }
        self.mark_dirty(x0, y0, x0 + w, y0 + h);
        self.cursor_x += w + 2 * pad;
        let uv = [
            x0 as f32 / self.width as f32,
            y0 as f32 / self.height as f32,
            (x0 + w) as f32 / self.width as f32,
            (y0 + h) as f32 / self.height as f32,
        ];
        // Center glyphs that are narrower than their cell allotment (fallback faces may have
        // a different advance than the primary face).
        let advance = metrics.advance_width;
        let slot = self.cell_w * cells as f32;
        let centering = if advance > 0.0 && (slot - advance).abs() > 0.5 {
            ((slot - advance) * 0.5).round()
        } else {
            0.0
        };
        Some(Glyph {
            uv,
            size: [w as f32, h as f32],
            offset: [
                metrics.xmin as f32 + centering,
                self.ascent - (metrics.ymin + metrics.height as i32) as f32,
            ],
            cells,
        })
    }

    fn reset_atlas(&mut self) {
        let _s = trace::span("atlas-reset");
        self.pixels.iter_mut().for_each(|p| *p = 0);
        self.glyphs.clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.reserve_white();
        self.mark_dirty(0, 0, self.width, self.height);
        self.reset = true;
    }
}

static GAMMA_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // (i/255)^(1/1.45) * 255, evaluated without floating point pow in const context via
        // a small fixed-point Newton iteration would be overkill; a quadratic-ish blend is
        // visually indistinguishable at these sizes.
        let x = i as u32;
        let boosted = x + ((x * (255 - x)) * 3) / (255 * 4);
        lut[i] = if boosted > 255 { 255 } else { boosted as u8 };
        i += 1;
    }
    lut
};
