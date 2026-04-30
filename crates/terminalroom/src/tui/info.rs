use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::tab_block;
use crate::app::{App, FileMeta};

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    // Image Info is read-only — never gets focus highlight.
    let block = tab_block("Image Info", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(entry) = app.current() else {
        return;
    };

    let lines: Vec<Line> = match app.file_meta.get(&entry.file.canonical_path) {
        Some(meta) => meta_lines(meta, &entry.file.display_name, inner.width as usize),
        None => vec![Line::from(Span::styled(
            "loading…",
            Style::default().add_modifier(Modifier::DIM),
        ))],
    };
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(p, inner);
}

fn meta_lines(meta: &FileMeta, display_name: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(section("Shoot"));
    push_kv(&mut lines, "Make", meta.shot_info.make.as_deref());
    push_kv(&mut lines, "Model", meta.shot_info.model.as_deref());
    if let Some(iso) = meta.shot_info.iso {
        let n = iso.round() as i64;
        lines.push(kv_line("ISO", &n.to_string()));
    }
    if let Some(s) = meta.shot_info.shutter {
        lines.push(kv_line("Shutter", &format_shutter(s)));
    }
    if let Some(a) = meta.shot_info.aperture {
        lines.push(kv_line("Aperture", &format!("f/{:.1}", a)));
    }
    if let Some(f) = meta.shot_info.focal_length {
        lines.push(kv_line("Focal", &format!("{:.0} mm", f)));
    }

    lines.push(Line::from(""));
    lines.push(section("File"));
    lines.push(kv_line("Name", &truncate(display_name, width.saturating_sub(7))));
    lines.push(kv_line("Format", meta.kind.label()));
    lines.push(kv_line("Size", &format_bytes(meta.size_bytes)));
    lines.push(kv_line(
        "Dims",
        &format!("{} × {}", meta.width, meta.height),
    ));
    if let Some(o) = meta.orientation {
        if o != 1 {
            lines.push(kv_line("Orient.", &format!("EXIF {o}")));
        }
    }

    lines
}

fn section(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn push_kv(lines: &mut Vec<Line<'static>>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        if !v.is_empty() {
            lines.push(kv_line(key, v));
        }
    }
}

fn kv_line(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{:<8}", key),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

fn format_shutter(seconds: f32) -> String {
    if seconds <= 0.0 {
        return "—".to_string();
    }
    if seconds >= 1.0 {
        format!("{:.1}s", seconds)
    } else {
        let denom = (1.0 / seconds).round() as i64;
        format!("1/{}", denom)
    }
}

fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    let cut: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutter_formats_fractional_and_seconds() {
        assert_eq!(format_shutter(1.0 / 250.0), "1/250");
        assert_eq!(format_shutter(2.0), "2.0s");
    }

    #[test]
    fn bytes_format_in_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }
}
