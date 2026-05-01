use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;

use darkroom::{
    DevelopParams, IDENTITY_ID, ImageKind, LookRegistry, ShotInfo, XmpRecipe, parse_xmp,
};

use crate::db::{Db, FileRow, LookRow};
use crate::session::{DiscoveredFile, Session};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Main,
    Filter,
    Looks,
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
    pub removed: bool,
    /// Persisted per-file develop knob values. The currently-selected entry's
    /// `develop_params` is mirrored into `App.develop_params` for live editing.
    pub develop_params: DevelopParams,
    pub develop_params_fp: u64,
    pub source_fp: u64,
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
    ("Look", DevelopKnob::LookSelector),
    ("Look Strength", DevelopKnob::LookStrength),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevelopKnob {
    ExposureEv,
    TemperatureKelvin,
    Tint,
    /// Discrete cycle through registered looks. Adjust cycles the slug;
    /// `step`/`read`/`write` are inert (the App branches on `is_discrete`).
    LookSelector,
    LookStrength,
}

impl DevelopKnob {
    pub fn step(self) -> f32 {
        match self {
            Self::ExposureEv => 0.05,
            Self::TemperatureKelvin => 100.0,
            Self::LookSelector => 0.0,
            _ => 0.05,
        }
    }
    pub fn read(self, p: &DevelopParams) -> f32 {
        match self {
            Self::ExposureEv => p.exposure_ev,
            Self::TemperatureKelvin => p.temperature_kelvin,
            Self::Tint => p.tint,
            Self::LookSelector => 0.0,
            Self::LookStrength => p.look_strength,
        }
    }
    pub fn write(self, p: &mut DevelopParams, v: f32) {
        match self {
            Self::ExposureEv => p.exposure_ev = v.clamp(-3.0, 3.0),
            Self::TemperatureKelvin => p.temperature_kelvin = v.clamp(2000.0, 12000.0),
            Self::Tint => p.tint = v.clamp(-1.0, 1.0),
            Self::LookSelector => {} // discrete; cycled by App::develop_adjust
            Self::LookStrength => p.look_strength = v.clamp(0.0, 1.0),
        }
    }
    /// Format the value for display. `looks` is consulted only for
    /// `LookSelector` to resolve the current slug to a display name.
    pub fn format(self, p: &DevelopParams, looks: &[LookRow]) -> String {
        match self {
            Self::ExposureEv => format!("{:+.2} EV", p.exposure_ev),
            Self::TemperatureKelvin => format!("{:>5.0} K", p.temperature_kelvin),
            Self::Tint => format!("{:+.2}", p.tint),
            Self::LookSelector => format_look_value(p, looks),
            Self::LookStrength => format!("{:+.2}", p.look_strength),
        }
    }
    pub fn is_discrete(self) -> bool {
        matches!(self, Self::LookSelector)
    }
}

fn format_look_value(p: &DevelopParams, looks: &[LookRow]) -> String {
    if p.look == IDENTITY_ID {
        return "(none)".to_string();
    }
    looks
        .iter()
        .find(|r| r.slug == p.look)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| "(missing)".to_string())
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
    /// Live editing buffer for the currently-selected file's knobs. Mirrors
    /// `files[visible[cursor]].develop_params` between flushes.
    pub develop_params: DevelopParams,
    pub develop_cursor: usize,
    pub file_meta: HashMap<PathBuf, FileMeta>,
    pub show_removed: bool,
    /// All registered XMP-driven looks, sorted by name. Refreshed by
    /// `reconcile_looks_with_dir`. Cursor 0 in the Looks modal is the
    /// `(none)/identity` pseudo-entry; cursor `i+1` corresponds to `looks[i]`.
    pub looks: Vec<LookRow>,
    pub looks_cursor: usize,
    /// Runtime registry feeding `apply_pipeline`. Replaced (Arc-on-write)
    /// every reconcile; in-flight Jobs keep their cloned Arc.
    pub look_registry: Arc<LookRegistry>,
}

impl App {
    pub fn init(session: Session, db: Db, rows: Vec<FileRow>) -> Result<Self> {
        let mut row_by_path: BTreeMap<PathBuf, FileRow> = BTreeMap::new();
        for r in rows {
            row_by_path.insert(r.canonical_path.clone(), r);
        }

        let mut files: Vec<FileEntry> = Vec::with_capacity(session.files.len());
        for f in session.files {
            // Every discovered file should have been upserted before init.
            let row = row_by_path.remove(&f.canonical_path).unwrap_or_else(|| {
                // Defensive default — should never happen.
                FileRow {
                    id: 0,
                    canonical_path: f.canonical_path.clone(),
                    size_bytes: f.size_bytes,
                    modified_unix_seconds: f.modified_unix_seconds,
                    source_fp: 0,
                    removed: false,
                    develop_params: DevelopParams::default(),
                    develop_params_fp: DevelopParams::default().fingerprint(),
                    cache_key: None,
                }
            });
            files.push(FileEntry {
                id: row.id,
                file: f,
                removed: row.removed,
                develop_params: row.develop_params,
                develop_params_fp: row.develop_params_fp,
                source_fp: row.source_fp,
            });
        }

        let mut counts: BTreeMap<ImageKind, usize> = BTreeMap::new();
        for entry in &files {
            *counts.entry(entry.file.kind).or_insert(0) += 1;
        }
        let mut available_formats: Vec<(ImageKind, usize)> = counts.into_iter().collect();
        available_formats.sort_by_key(|(k, _)| k.label());
        let enabled_formats: BTreeSet<ImageKind> =
            available_formats.iter().map(|(k, _)| *k).collect();

        let initial_params = files
            .first()
            .map(|e| e.develop_params.clone())
            .unwrap_or_default();

        // Seed the look registry from any persisted rows. The TUI calls
        // `reconcile_looks_with_dir` on every modal-open to pick up filesystem
        // changes; init only loads what's already in the DB.
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
            develop_params: initial_params,
            develop_cursor: 0,
            file_meta: HashMap::new(),
            show_removed: false,
            looks: Vec::new(),
            looks_cursor: 0,
            look_registry: Arc::new(LookRegistry::new()),
        };
        app.refresh_looks_from_db();
        app.rebuild_visible_keep_path(None);
        app.sync_develop_params_from_current();
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

    /// Adjust the focused knob by `direction * step`, or for the discrete
    /// `LookSelector` knob, cycle through registered looks.
    pub fn develop_adjust(&mut self, direction: f32) {
        let Some(&(_, knob)) = DEVELOP_KNOBS.get(self.develop_cursor) else {
            return;
        };
        if knob == DevelopKnob::LookSelector {
            self.look_selector_cycle(direction.signum() as i32);
            return;
        }
        let current = knob.read(&self.develop_params);
        let next = current + direction * knob.step();
        knob.write(&mut self.develop_params, next);
    }

    /// Reset the focused knob to its default value (or to `identity` for the
    /// `LookSelector`).
    pub fn develop_reset(&mut self) {
        let Some(&(_, knob)) = DEVELOP_KNOBS.get(self.develop_cursor) else {
            return;
        };
        match knob {
            DevelopKnob::LookSelector => {
                self.develop_params.look = IDENTITY_ID.to_string();
            }
            _ => {
                let default = knob.read(&DevelopParams::default());
                knob.write(&mut self.develop_params, default);
            }
        }
    }

    fn look_selector_cycle(&mut self, direction: i32) {
        if direction == 0 {
            return;
        }
        let total = 1 + self.looks.len();
        if total == 0 {
            return;
        }
        let current_idx = if self.develop_params.look == IDENTITY_ID {
            0
        } else {
            self.looks
                .iter()
                .position(|r| r.slug == self.develop_params.look)
                .map(|p| p + 1)
                .unwrap_or(0)
        };
        let next = if direction > 0 {
            (current_idx + 1) % total
        } else {
            (current_idx + total - 1) % total
        };
        self.develop_params.look = if next == 0 {
            IDENTITY_ID.to_string()
        } else {
            self.looks[next - 1].slug.clone()
        };
    }

    /// Replace `self.develop_params` with the current file's persisted params.
    /// Call after any selection change so live knob editing tracks the active
    /// row, and the in-memory + on-disk caches keyed by the current params
    /// fingerprint are consulted correctly.
    pub fn sync_develop_params_from_current(&mut self) {
        if let Some(entry) = self.current() {
            self.develop_params = entry.develop_params.clone();
            self.develop_cursor = self.develop_cursor.min(DEVELOP_KNOBS.len() - 1);
        }
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
        let show_removed = self.show_removed;
        self.visible = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                self.enabled_formats.contains(&e.file.kind) && (show_removed || !e.removed)
            })
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
            self.sync_develop_params_from_current();
        }
    }

    pub fn prev(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.sync_develop_params_from_current();
        }
    }

    pub fn current(&self) -> Option<&FileEntry> {
        self.visible.get(self.cursor).map(|&i| &self.files[i])
    }

    /// Mark the current entry as removed and persist immediately.
    pub fn remove_current(&mut self, now_unix: i64) {
        let Some(&i) = self.visible.get(self.cursor) else {
            return;
        };
        if self.files[i].removed {
            return;
        }
        let file_id = self.files[i].id;
        match self.db.set_removed(file_id, true, now_unix) {
            Ok(()) => {
                self.files[i].removed = true;
                self.status = None;
                self.rebuild_visible();
                self.sync_develop_params_from_current();
            }
            Err(e) => {
                self.status = Some(format!("failed to remove: {e}"));
            }
        }
    }

    /// Restore the current entry (no-op if not removed).
    pub fn restore_current(&mut self, now_unix: i64) {
        let Some(&i) = self.visible.get(self.cursor) else {
            return;
        };
        if !self.files[i].removed {
            return;
        }
        let file_id = self.files[i].id;
        match self.db.set_removed(file_id, false, now_unix) {
            Ok(()) => {
                self.files[i].removed = false;
                self.status = None;
                self.rebuild_visible();
                self.sync_develop_params_from_current();
            }
            Err(e) => {
                self.status = Some(format!("failed to restore: {e}"));
            }
        }
    }

    pub fn toggle_show_removed(&mut self) {
        self.show_removed = !self.show_removed;
        self.rebuild_visible();
        self.sync_develop_params_from_current();
    }

    /// Mirror the in-memory `develop_params` into the current `FileEntry`
    /// (called after a successful debounced flush).
    pub fn commit_develop_params(&mut self, fp: u64) {
        if let Some(&i) = self.visible.get(self.cursor) {
            self.files[i].develop_params = self.develop_params.clone();
            self.files[i].develop_params_fp = fp;
        }
    }

    pub fn toggle_format(&mut self, kind: ImageKind) {
        if !self.enabled_formats.remove(&kind) {
            self.enabled_formats.insert(kind);
        }
        self.rebuild_visible();
        self.sync_develop_params_from_current();
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

    /// Open the Looks modal. Reconciles the watch dir first so the list is
    /// up-to-date on every open. Caller passes `paths::looks_dir()`.
    pub fn open_looks(&mut self, looks_dir: &Path, now_unix: i64) {
        self.reconcile_looks_with_dir(looks_dir, now_unix);
        self.looks_cursor = if self.develop_params.look == IDENTITY_ID {
            0
        } else {
            self.looks
                .iter()
                .position(|r| r.slug == self.develop_params.look)
                .map(|p| p + 1)
                .unwrap_or(0)
        };
        self.view = View::Looks;
    }

    pub fn close_looks(&mut self) {
        self.view = View::Main;
    }

    pub fn looks_next(&mut self) {
        let total = 1 + self.looks.len();
        if self.looks_cursor + 1 < total {
            self.looks_cursor += 1;
        }
    }

    pub fn looks_prev(&mut self) {
        if self.looks_cursor > 0 {
            self.looks_cursor -= 1;
        }
    }

    /// Apply the look at `looks_cursor` to the current file's live params.
    /// The TUI's existing dirty-flag check picks up the slug change and
    /// triggers the standard debounce → save-worker flush.
    pub fn looks_apply_to_current(&mut self) {
        let slug = if self.looks_cursor == 0 {
            IDENTITY_ID.to_string()
        } else {
            match self.looks.get(self.looks_cursor - 1) {
                Some(r) => r.slug.clone(),
                None => return,
            }
        };
        self.develop_params.look = slug;
    }

    /// Walk `dir` for `*.xmp` files; insert any unregistered ones into the DB,
    /// prune DB rows whose `source_path` no longer exists, then refresh
    /// `self.looks` and `self.look_registry`. Errors are surfaced via
    /// `self.status` and never panic.
    pub fn reconcile_looks_with_dir(&mut self, dir: &Path, now_unix: i64) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let is_xmp = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("xmp"))
                    .unwrap_or(false);
                if !is_xmp {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let size = meta.len() as i64;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let fp = crate::db::compute_source_fp(size, mtime);
                match self.db.find_look_by_fp(fp) {
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        let xml = match std::fs::read_to_string(&path) {
                            Ok(s) => s,
                            Err(e) => {
                                self.status = Some(format!(
                                    "xmp read failed for {}: {e}",
                                    path.display()
                                ));
                                continue;
                            }
                        };
                        let recipe = match parse_xmp(&xml) {
                            Ok(r) => r,
                            Err(e) => {
                                self.status = Some(format!(
                                    "xmp parse failed for {}: {e}",
                                    path.display()
                                ));
                                continue;
                            }
                        };
                        let _ = recipe; // recipe gets re-parsed when we rebuild the registry below
                        let slug = format!("xmp:{:016x}", fp);
                        let name = derive_look_name(&xml, &path);
                        if let Err(e) = self
                            .db
                            .insert_look(&slug, &name, &path, &xml, fp, now_unix)
                        {
                            self.status = Some(format!("xmp insert failed: {e}"));
                        }
                    }
                    Err(e) => {
                        self.status = Some(format!("xmp lookup failed: {e}"));
                    }
                }
            }
        }

        // Prune DB rows whose source file is gone.
        let rows = match self.db.list_looks() {
            Ok(r) => r,
            Err(e) => {
                self.status = Some(format!("list_looks failed: {e}"));
                return;
            }
        };
        for row in &rows {
            if !row.source_path.exists() {
                let _ = self.db.delete_look(row.id);
            }
        }

        self.refresh_looks_from_db();
    }

    /// Reload `self.looks` from the DB and rebuild `self.look_registry`.
    /// Parse failures on stored XML are silently skipped (the row stays in
    /// the DB but resolves to Identity at apply time).
    fn refresh_looks_from_db(&mut self) {
        self.looks = self.db.list_looks().unwrap_or_default();
        let mut reg = LookRegistry::new();
        for row in &self.looks {
            if let Ok(recipe) = parse_xmp(&row.xmp_xml) {
                reg.register_xmp(row.slug.clone(), recipe);
            }
        }
        self.look_registry = Arc::new(reg);
    }
}

fn derive_look_name(xml: &str, path: &Path) -> String {
    if let Ok(recipe) = parse_xmp(xml)
        && let Some(name) = recipe.name
        && !name.is_empty()
    {
        return name;
    }
    let _ = XmpRecipe::default(); // suppress dead-code if parse_xmp gets gated
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string()
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
        let mut rows = Vec::with_capacity(files.len());
        for f in &files {
            rows.push(db.upsert_file(f, 1000).unwrap());
        }
        let session = Session {
            root: PathBuf::from("/t"),
            files,
        };
        App::init(session, db, rows).unwrap()
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
        assert!(!app.show_removed);
        assert!(app.looks.is_empty());
        let labels: Vec<_> = app
            .available_formats
            .iter()
            .map(|(k, n)| (k.label(), *n))
            .collect();
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
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn remove_current_filters_from_visible() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.cr3", ImageKind::Raw),
        ]);
        app.remove_current(1000);
        assert!(app.files[0].removed);
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.current().unwrap().file.display_name, "b.cr3");
    }

    #[test]
    fn toggle_show_removed_reveals_and_hides() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.cr3", ImageKind::Raw),
        ]);
        app.remove_current(1000);
        assert_eq!(app.visible.len(), 1);
        app.toggle_show_removed();
        assert_eq!(app.visible.len(), 2);
        // The removed file is now reachable.
        app.cursor = 0;
        assert_eq!(app.current().unwrap().file.display_name, "a.cr3");
        assert!(app.current().unwrap().removed);
        app.toggle_show_removed();
        assert_eq!(app.visible.len(), 1);
    }

    #[test]
    fn restore_current_undoes_remove() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.cr3", ImageKind::Raw),
        ]);
        app.remove_current(1000);
        app.toggle_show_removed();
        app.cursor = 0; // a.cr3 (removed)
        app.restore_current(2000);
        assert!(!app.files[0].removed);
        app.toggle_show_removed();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn restore_is_noop_when_not_removed() {
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        let before = app.files[0].removed;
        app.restore_current(1000);
        assert_eq!(app.files[0].removed, before);
    }

    #[test]
    fn selecting_other_file_swaps_develop_params() {
        let mut app = build_app(vec![
            file("a.cr3", ImageKind::Raw),
            file("b.cr3", ImageKind::Raw),
        ]);
        // Edit + commit params for file A.
        app.develop_params.exposure_ev = 1.5;
        let fp = app.develop_params.fingerprint();
        app.commit_develop_params(fp);
        // Move to B — params should reset to default.
        app.next();
        assert_eq!(app.develop_params.exposure_ev, 0.0);
        // Move back to A — restored.
        app.prev();
        assert_eq!(app.develop_params.exposure_ev, 1.5);
    }

    #[test]
    fn enter_exit_develop_toggles_focus() {
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        assert_eq!(app.focus, Focus::Navigation);
        app.enter_develop();
        assert_eq!(app.focus, Focus::Develop);
        assert_eq!(app.view, View::Main);
        app.exit_develop();
        assert_eq!(app.focus, Focus::Navigation);
    }

    fn write_xmp(path: &Path, name: &str) -> u64 {
        // Minimal valid-enough XMP: just <crs:Name> inside an <rdf:Description>.
        let xml = format!(
            r#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" rdf:about="">
   <crs:Name>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">{name}</rdf:li>
    </rdf:Alt>
   </crs:Name>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
        );
        std::fs::write(path, &xml).unwrap();
        let meta = std::fs::metadata(path).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        crate::db::compute_source_fp(meta.len() as i64, mtime)
    }

    #[test]
    fn reconcile_looks_with_dir_inserts_new_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        let xmp = tmp.path().join("preset-a.xmp");
        let fp = write_xmp(&xmp, "Preset A");

        app.reconcile_looks_with_dir(tmp.path(), 1234);
        assert_eq!(app.looks.len(), 1);
        let row = &app.looks[0];
        assert_eq!(row.name, "Preset A");
        assert_eq!(row.slug, format!("xmp:{:016x}", fp));
        assert!(app.look_registry.is_registered(&row.slug));
    }

    #[test]
    fn reconcile_looks_with_dir_prunes_missing_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        let xmp = tmp.path().join("p.xmp");
        write_xmp(&xmp, "P");
        app.reconcile_looks_with_dir(tmp.path(), 1234);
        assert_eq!(app.looks.len(), 1);
        std::fs::remove_file(&xmp).unwrap();
        app.reconcile_looks_with_dir(tmp.path(), 1235);
        assert!(app.looks.is_empty());
    }

    #[test]
    fn reconcile_is_idempotent_on_unchanged_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        let xmp = tmp.path().join("p.xmp");
        write_xmp(&xmp, "P");
        app.reconcile_looks_with_dir(tmp.path(), 1234);
        let before = app.looks.clone();
        app.reconcile_looks_with_dir(tmp.path(), 5678);
        assert_eq!(before, app.looks);
    }

    #[test]
    fn looks_apply_to_current_sets_develop_params_look() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        // Names of different lengths — the source fingerprint hashes
        // (size, mtime), so equal-sized files written in the same second
        // would collide and dedupe to one entry.
        write_xmp(&tmp.path().join("look-a.xmp"), "First Look");
        write_xmp(&tmp.path().join("look-b.xmp"), "Second");
        app.reconcile_looks_with_dir(tmp.path(), 1234);
        assert_eq!(app.looks.len(), 2);

        app.looks_cursor = 1; // first registered look
        app.looks_apply_to_current();
        assert_eq!(app.develop_params.look, app.looks[0].slug);

        app.looks_cursor = 0; // identity
        app.looks_apply_to_current();
        assert_eq!(app.develop_params.look, IDENTITY_ID);
    }

    #[test]
    fn look_selector_cycles_forward_then_back() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = build_app(vec![file("a.cr3", ImageKind::Raw)]);
        write_xmp(&tmp.path().join("look-a.xmp"), "Alpha");
        write_xmp(&tmp.path().join("look-b.xmp"), "Bravo Echo");
        app.reconcile_looks_with_dir(tmp.path(), 1234);
        assert_eq!(app.looks.len(), 2);

        // Position cursor on the LookSelector knob.
        app.develop_cursor = DEVELOP_KNOBS
            .iter()
            .position(|(_, k)| *k == DevelopKnob::LookSelector)
            .unwrap();

        assert_eq!(app.develop_params.look, IDENTITY_ID);
        app.develop_adjust(1.0);
        assert_eq!(app.develop_params.look, app.looks[0].slug);
        app.develop_adjust(1.0);
        assert_eq!(app.develop_params.look, app.looks[1].slug);
        app.develop_adjust(1.0); // wrap to identity
        assert_eq!(app.develop_params.look, IDENTITY_ID);
        app.develop_adjust(-1.0); // wrap back to last
        assert_eq!(app.develop_params.look, app.looks[1].slug);

        app.develop_reset();
        assert_eq!(app.develop_params.look, IDENTITY_ID);
    }
}
