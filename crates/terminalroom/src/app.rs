use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Result, bail};

use darkroom::format::ImageKind;

use crate::db::{CullingState, Db, FileRecord};
use crate::session::{DiscoveredFile, Session};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Culling,
    Develop,
    Filter,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub id: i64,
    pub file: DiscoveredFile,
    pub state: CullingState,
}

pub struct App {
    pub session_root: PathBuf,
    pub files: Vec<FileEntry>,
    pub visible: Vec<usize>,
    pub cursor: usize,
    pub view: View,
    pub enabled_formats: BTreeSet<ImageKind>,
    pub available_formats: Vec<(ImageKind, usize)>,
    pub filter_cursor: usize,
    pub status: Option<String>,
    pub db: Db,
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
            files.push(FileEntry {
                id,
                file: f,
                state,
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

        // Drop owned Db to silence the "mut" warning when not yet used; we hand it back.
        let _ = &mut db;

        let mut app = Self {
            session_root: session.root,
            files,
            visible: Vec::new(),
            cursor: 0,
            view: View::Culling,
            enabled_formats,
            available_formats,
            filter_cursor: 0,
            status: None,
            db,
        };
        app.rebuild_visible_keep_path(None);
        Ok(app)
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
        self.view = View::Culling;
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
        let labels: Vec<_> = app
            .available_formats
            .iter()
            .map(|(k, n)| (k.label(), *n))
            .collect();
        // sorted by label: JPEG, PNG, RAW
        assert_eq!(
            labels,
            vec![("JPEG", 1), ("PNG", 1), ("RAW", 1)]
        );
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
        assert_eq!(app.view, View::Culling);
    }
}
