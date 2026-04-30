use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use anyhow::{Result, bail};

use darkroom::{DevelopParams, ImageKind, ShotInfo};

use crate::db::{CullingState, Db, FileRecord};
use crate::session::{DiscoveredFile, Session};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Main,
    Filter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Navigation,
    Develop,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub id: i64,
    pub file: DiscoveredFile,
    pub state: CullingState,
}

/// Cached header metadata for the Image Info tab. Populated lazily by the
/// preview worker as it decodes each file.
#[derive(Clone, Debug)]
pub struct FileMeta {
    pub shot_info: ShotInfo,
    pub width: u32,
    pub height: u32,
    /// EXIF orientation code (1..=8) for image-format files; `None` for RAW
    /// (libraw applies orientation internally).
    pub orientation: Option<u16>,
    pub size_bytes: u64,
    pub kind: ImageKind,
}

/// Knobs presented in the develop view, in display order. Each entry is
/// `(label, kind)`; the cursor indexes into this slice.
pub const DEVELOP_KNOBS: &[(&str, DevelopKnob)] = &[
    ("Exposure", DevelopKnob::ExposureEv),
    ("Temperature", DevelopKnob::TemperatureKelvin),
    ("Tint", DevelopKnob::Tint),
    ("Look Strength", DevelopKnob::LookStrength),
    ("Warmth", DevelopKnob::Warmth),
    ("Color", DevelopKnob::Color),
    ("Contrast", DevelopKnob::Contrast),
    ("Soft Highlights", DevelopKnob::SoftHighlights),
    ("Shadows", DevelopKnob::Shadows),
    ("Blacks", DevelopKnob::Blacks),
    ("Clarity", DevelopKnob::Clarity),
    ("Grain", DevelopKnob::Grain),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevelopKnob {
    ExposureEv,
    TemperatureKelvin,
    Tint,
    LookStrength,
    Warmth,
    Color,
    Contrast,
    SoftHighlights,
    Shadows,
    Blacks,
    Clarity,
    Grain,
}

impl DevelopKnob {
    pub fn step(self) -> f32 {
        match self {
            Self::ExposureEv => 0.05,
            Self::TemperatureKelvin => 100.0,
            _ => 0.05,
        }
    }
    pub fn read(self, p: &DevelopParams) -> f32 {
        match self {
            Self::ExposureEv => p.exposure_ev,
            Self::TemperatureKelvin => p.temperature_kelvin,
            Self::Tint => p.tint,
            Self::LookStrength => p.look_strength,
            Self::Warmth => p.warmth,
            Self::Color => p.color,
            Self::Contrast => p.contrast,
            Self::SoftHighlights => p.soft_highlights,
            Self::Shadows => p.shadows,
            Self::Blacks => p.blacks,
            Self::Clarity => p.clarity,
            Self::Grain => p.grain,
        }
    }
    pub fn write(self, p: &mut DevelopParams, v: f32) {
        match self {
            Self::ExposureEv => p.exposure_ev = v.clamp(-3.0, 3.0),
            Self::TemperatureKelvin => p.temperature_kelvin = v.clamp(2000.0, 12000.0),
            Self::Tint => p.tint = v.clamp(-1.0, 1.0),
            Self::LookStrength => p.look_strength = v.clamp(0.0, 1.0),
            Self::Warmth => p.warmth = v.clamp(-1.0, 1.0),
            Self::Color => p.color = v.clamp(-1.0, 1.0),
            Self::Contrast => p.contrast = v.clamp(-1.0, 1.0),
            Self::SoftHighlights => p.soft_highlights = v.clamp(0.0, 1.0),
            Self::Shadows => p.shadows = v.clamp(-1.0, 1.0),
            Self::Blacks => p.blacks = v.clamp(-1.0, 1.0),
            Self::Clarity => p.clarity = v.clamp(-1.0, 1.0),
            Self::Grain => p.grain = v.clamp(0.0, 1.0),
        }
    }
    /// Format the value for display.
    pub fn format(self, p: &DevelopParams) -> String {
        match self {
            Self::ExposureEv => format!("{:+.2} EV", p.exposure_ev),
            Self::TemperatureKelvin => format!("{:>5.0} K", p.temperature_kelvin),
            _ => format!("{:+.2}", self.read(p)),
        }
    }
}

pub struct App {
    pub session_root: PathBuf,
    pub files: Vec<FileEntry>,
    pub visible: Vec<usize>,
    pub cursor: usize,
    pub view: View,
    pub focus: Focus,
    pub enabled_formats: BTreeSet<ImageKind>,
    pub available_formats: Vec<(ImageKind, usize)>,
    pub filter_cursor: usize,
    pub status: Option<String>,
    pub db: Db,
    pub develop_params: DevelopParams,
    pub develop_cursor: usize,
    pub file_meta: HashMap<PathBuf, FileMeta>,
}

impl App {
    pub fn init(session: Session, mut db: Db, records: Vec<FileRecord>) -> Result<Self> {
        let mut state_by_path: BTreeMap<PathBuf, (i64, CullingState)> = BTreeMap::new();
        for r in records {
            state_by_path.insert(r.canonical_path.clone(), (r.id, r.state));
        }

        let mut files: Vec<FileEntry> = Vec::with_capacity(session.files.len());
        for f in session.files {
            let Some((id, state)) = state_by_path.remove(&f.canonical_path) else {
                bail!(
                    "internal: discovered file {} missing from sync_files records",
                    f.canonical_path.display()
                );
            };
            files.push(FileEntry { id, file: f, state });
        }

        let mut counts: BTreeMap<ImageKind, usize> = BTreeMap::new();
        for entry in &files {
            *counts.entry(entry.file.kind).or_insert(0) += 1;
        }
        let mut available_formats: Vec<(ImageKind, usize)> = counts.into_iter().collect();
        available_formats.sort_by_key(|(k, _)| k.label());
        let enabled_formats: BTreeSet<ImageKind> =
            available_formats.iter().map(|(k, _)| *k).collect();

        // Drop owned Db to silence the "mut" warning when not yet used; we hand it back.
        let _ = &mut db;

        let mut app = Self {
            session_root: session.root,
            files,
            visible: Vec::new(),
            cursor: 0,
            view: View::Main,
            focus: Focus::Navigation,
            enabled_formats,
            available_formats,
            filter_cursor: 0,
            status: None,
            db,
            develop_params: DevelopParams::default(),
            develop_cursor: 0,
            file_meta: HashMap::new(),
        };
        app.rebuild_visible_keep_path(None);
        Ok(app)
    }

    pub fn enter_develop(&mut self) {
        self.focus = Focus::Develop;
    }

    pub fn exit_develop(&mut self) {
        self.focus = Focus::Navigation;
    }

    pub fn develop_next(&mut self) {
        if self.develop_cursor + 1 < DEVELOP_KNOBS.len() {
            self.develop_cursor += 1;
        }
    }

    pub fn develop_prev(&mut self) {
        if self.develop_cursor > 0 {
            self.develop_cursor -= 1;
        }
    }

    /// Adjust the focused knob by `direction * step`.
    pub fn develop_adjust(&mut self, direction: f32) {
        let Some(&(_, knob)) = DEVELOP_KNOBS.get(self.develop_cursor) else {
            return;
        };
        let current = knob.read(&self.develop_params);
        let next = current + direction * knob.step();
        knob.write(&mut self.develop_params, next);
    }

    /// Reset the focused knob to its default value.
    pub fn develop_reset(&mut self) {
        let Some(&(_, knob)) = DEVELOP_KNOBS.get(self.develop_cursor) else {
            return;
        };
        let default = knob.read(&DevelopParams::default());
        knob.write(&mut self.develop_params, default);
    }

    pub fn rebuild_visible(&mut self) {
        let current_path = self
            .visible
            .get(self.cursor)
            .and_then(|&i| self.files.get(i))
            .map(|e| e.file.canonical_path.clone());
        self.rebuild_visible_keep_path(current_path);
    }

    fn rebuild_visible_keep_path(&mut self, target: Option<PathBuf>) {
        self.visible = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, e)| self.enabled_formats.contains(&e.file.kind))
            .map(|(i, _)| i)
            .collect();

        if self.visible.is_empty() {
            self.cursor = 0;
            return;
        }

        if let Some(path) = target {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.files[i].file.canonical_path == path)
            {
                self.cursor = pos;
                return;
            }
        }

        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    pub fn next(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        if self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    pub fn prev(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn current(&self) -> Option<&FileEntry> {
        self.visible.get(self.cursor).map(|&i| &self.files[i])
    }

    pub fn set_state(&mut self, state: CullingState, now_unix: i64) {
        let Some(&i) = self.visible.get(self.cursor) else {
            return;
        };
        let file_id = self.files[i].id;
        match self.db.set_state(file_id, state, now_unix) {
            Ok(()) => {
                self.files[i].state = state;
                self.status = None;
            }
            Err(e) => {
                self.status = Some(format!("failed to save state: {e}"));
            }
        }
    }

    pub fn toggle_format(&mut self, kind: ImageKind) {
        if !self.enabled_formats.remove(&kind) {
            self.enabled_formats.insert(kind);
        }
        self.rebuild_visible();
    }

    pub fn open_filter(&mut self) {
        if self.available_formats.is_empty() {
            return;
        }
        self.filter_cursor = self.filter_cursor.min(self.available_formats.len() - 1);
        self.view = View::Filter;
    }

    pub fn close_filter(&mut self) {
        self.view = View::Main;
    }

    pub fn filter_next(&mut self) {
        if self.available_formats.is_empty() {
            return;
        }
        if self.filter_cursor + 1 < self.available_formats.len() {
            self.filter_cursor += 1;
        }
    }

    pub fn filter_prev(&mut self) {
        if self.filter_cursor > 0 {
            self.filter_cursor -= 1;
        }
    }

    pub fn toggle_current_filter(&mut self) {
        if let Some(&(kind, _)) = self.available_formats.get(self.filter_cursor) {
            self.toggle_format(kind);
        }
    }

    pub fn enabled_count(&self) -> usize {
        self.enabled_formats.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::session::{DiscoveredFile, Session};
    use std::path::PathBuf;

    fn file(name: &str, kind: ImageKind) -> DiscoveredFile {
        DiscoveredFile {
            canonical_path: PathBuf::from(format!("/t/{name}")),
            display_name: name.to_string(),
            size_bytes: 100,
            modified_unix_seconds: 1000,
            kind,
        }
    }

    fn build_app(files: Vec<DiscoveredFile>) -> App {
        let mut db = Db::open_in_memory().unwrap();
        let records = db.sync_files(&files, 1000).unwrap();
        let session = Session {
            root: PathBuf::from("/t"),
            files,
        };
        App::init(session, db, records).unwrap()
    }

    #[test]
    fn init_seeds_visible_and_available_formats() {
        let app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.jpg", ImageKind::Jpeg),
            file("c.png", ImageKind::Png),
        ]);
        assert_eq!(app.visible.len(), 3);
        assert_eq!(app.cursor, 0);
        assert_eq!(app.enabled_formats.len(), 3);
        assert_eq!(app.view, View::Main);
        assert_eq!(app.focus, Focus::Navigation);
        let labels: Vec<_> = app
            .available_formats
            .iter()
            .map(|(k, n)| (k.label(), *n))
            .collect();
        // sorted by label: JPEG, PNG, RAW
        assert_eq!(labels, vec![("JPEG", 1), ("PNG", 1), ("RAW", 1)]);
    }

    #[test]
    fn toggle_format_hides_matching_files() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.jpg", ImageKind::Jpeg),
            file("c.cr3", ImageKind::Raw),
        ]);
        app.toggle_format(ImageKind::Raw);
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.current().unwrap().file.display_name, "b.jpg");
    }

    #[test]
    fn rebuild_keeps_selection_by_path() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.jpg", ImageKind::Jpeg),
            file("c.png", ImageKind::Png),
        ]);
        app.cursor = 2; // c.png
        app.toggle_format(ImageKind::Raw);
        // c.png still visible — selection follows it
        assert_eq!(app.current().unwrap().file.display_name, "c.png");
    }

    #[test]
    fn rebuild_clamps_when_selection_filtered_out() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.jpg", ImageKind::Jpeg),
        ]);
        app.cursor = 0; // a.cr3
        app.toggle_format(ImageKind::Raw);
        // a.cr3 hidden; cursor falls back to last visible (b.jpg at index 0)
        assert_eq!(app.cursor, 0);
        assert_eq!(app.current().unwrap().file.display_name, "b.jpg");
    }

    #[test]
    fn rebuild_handles_empty_visible() {
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        app.toggle_format(ImageKind::Raw);
        assert!(app.visible.is_empty());
        assert!(app.current().is_none());
        // ops on empty list are no-ops
        app.next();
        app.prev();
    }

    #[test]
    fn next_prev_no_wrap() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.cr3", ImageKind::Raw),
        ]);
        app.prev();
        assert_eq!(app.cursor, 0);
        app.next();
        assert_eq!(app.cursor, 1);
        app.next();
        assert_eq!(app.cursor, 1); // clamped at end
    }

    #[test]
    fn set_state_persists_and_clears_status() {
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        app.status = Some("prior error".into());
        app.set_state(CullingState::Pick, 2000);
        assert_eq!(app.current().unwrap().state, CullingState::Pick);
        assert!(app.status.is_none());
    }

    #[test]
    fn filter_navigation_and_toggle() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.jpg", ImageKind::Jpeg),
        ]);
        app.open_filter();
        assert_eq!(app.view, View::Filter);
        assert_eq!(app.filter_cursor, 0); // JPEG (sorted by label)
        app.filter_next();
        assert_eq!(app.filter_cursor, 1); // RAW
        app.filter_next();
        assert_eq!(app.filter_cursor, 1); // clamped
        app.toggle_current_filter(); // toggle off RAW
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.current().unwrap().file.display_name, "b.jpg");
        app.close_filter();
        assert_eq!(app.view, View::Main);
    }

    #[test]
    fn enter_exit_develop_toggles_focus() {
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        assert_eq!(app.focus, Focus::Navigation);
        app.enter_develop();
        assert_eq!(app.focus, Focus::Develop);
        // view stays Main while focus shifts.
        assert_eq!(app.view, View::Main);
        app.exit_develop();
        assert_eq!(app.focus, Focus::Navigation);
    }
}
