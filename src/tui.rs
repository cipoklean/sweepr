//! Terminal UI: browse both buckets, tick items, read notes, clean to quarantine, undo.

use crate::docker;
use crate::notes;
use crate::quarantine;
use crate::rules::Bucket;
use crate::scan::Item;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::path::PathBuf;

pub struct Row {
    pub item: Item,
    pub checked: bool,
    pub note: String,
}

#[derive(PartialEq, Debug)]
pub enum Mode {
    Browse,
    Confirm,
}

pub struct App {
    pub rows: Vec<Row>,
    pub tab: Bucket,
    lists: [ListState; 2],
    pub mode: Mode,
    pub status: String,
    base: PathBuf,
    last_batch: Option<(String, Vec<Row>)>,
    pub quit: bool,
}

impl App {
    pub fn new(items: Vec<Item>, base: PathBuf) -> Self {
        let rows = items
            .into_iter()
            .map(|item| Row {
                checked: item.bucket == Bucket::Safe,
                note: notes::builtin(&item),
                item,
            })
            .collect();
        let mut app = App {
            rows,
            tab: Bucket::Safe,
            lists: [ListState::default(), ListState::default()],
            mode: Mode::Browse,
            status: "Safe items are ticked. Review, then press c to clean.".into(),
            base,
            last_batch: None,
            quit: false,
        };
        app.lists[0].select(Some(0));
        app.lists[1].select(Some(0));
        app
    }

    fn tab_ix(&self) -> usize {
        if self.tab == Bucket::Safe { 0 } else { 1 }
    }

    /// Indexes into `rows` for the current tab, in display order.
    pub fn visible(&self) -> Vec<usize> {
        self.rows.iter().enumerate().filter(|(_, r)| r.item.bucket == self.tab).map(|(i, _)| i).collect()
    }

    pub fn current(&self) -> Option<usize> {
        let vis = self.visible();
        let sel = self.lists[self.tab_ix()].selected()?;
        vis.get(sel.min(vis.len().saturating_sub(1))).copied()
    }

    pub fn bytes(&self, bucket: Bucket) -> u64 {
        self.rows.iter().filter(|r| r.item.bucket == bucket).map(|r| r.item.bytes).sum()
    }

    pub fn selected_bytes(&self) -> u64 {
        self.rows.iter().filter(|r| r.checked).map(|r| r.item.bytes).sum()
    }

    fn can_tick(r: &Row) -> bool {
        // Docker volumes, containers and tagged images are never removed by Sweepr.
        !(r.item.kind.is_docker() && r.item.bucket == Bucket::NeedsYou)
    }

    pub fn toggle(&mut self) {
        let Some(i) = self.current() else { return };
        if !Self::can_tick(&self.rows[i]) {
            self.status = "Sweepr never removes Docker volumes, containers or tagged images. Use Docker for those.".into();
            return;
        }
        self.rows[i].checked = !self.rows[i].checked;
    }

    pub fn toggle_all(&mut self) {
        let vis = self.visible();
        let all = vis.iter().filter(|&&i| Self::can_tick(&self.rows[i])).all(|&i| self.rows[i].checked);
        for i in vis {
            if Self::can_tick(&self.rows[i]) {
                self.rows[i].checked = !all;
            }
        }
    }

    pub fn move_by(&mut self, d: i32) {
        let n = self.visible().len();
        let ix = self.tab_ix();
        if n == 0 {
            self.lists[ix].select(None);
            return;
        }
        let cur = self.lists[ix].selected().unwrap_or(0) as i32;
        self.lists[ix].select(Some((cur + d).clamp(0, n as i32 - 1) as usize));
    }

    pub fn switch_tab(&mut self) {
        self.tab = if self.tab == Bucket::Safe { Bucket::NeedsYou } else { Bucket::Safe };
        self.move_by(0);
    }

    pub fn confirm_text(&self) -> Option<String> {
        let files: Vec<&Row> = self.rows.iter().filter(|r| r.checked && !r.item.kind.is_docker()).collect();
        let dock: Vec<&Row> = self.rows.iter().filter(|r| r.checked && r.item.kind.is_docker()).collect();
        if files.is_empty() && dock.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !files.is_empty() {
            let b: u64 = files.iter().map(|r| r.item.bytes).sum();
            let review = files.iter().filter(|r| r.item.bucket == Bucket::NeedsYou).count();
            let extra = if review > 0 { format!(", including {review} you reviewed") } else { String::new() };
            parts.push(format!("Move {} items ({}){extra} to quarantine. Undo for {} days.", files.len(), human(b), quarantine::KEEP_DAYS));
        }
        if !dock.is_empty() {
            let b: u64 = dock.iter().map(|r| r.item.bytes).sum();
            parts.push(format!("Prune {} of Docker data. This can't be undone.", human(b)));
        }
        Some(parts.join("\n"))
    }

    pub fn ask_clean(&mut self) {
        if self.confirm_text().is_some() {
            self.mode = Mode::Confirm;
        } else {
            self.status = "Nothing ticked.".into();
        }
    }

    pub fn clean(&mut self) {
        self.mode = Mode::Browse;
        let (picked, kept): (Vec<Row>, Vec<Row>) = std::mem::take(&mut self.rows).into_iter().partition(|r| r.checked);
        self.rows = kept;
        let (dock, files): (Vec<Row>, Vec<Row>) = picked.into_iter().partition(|r| r.item.kind.is_docker());
        let mut msgs = Vec::new();

        if !files.is_empty() {
            let list: Vec<_> = files.iter().map(|r| (r.item.path.clone(), r.item.bytes, r.item.kind.label().to_string())).collect();
            match quarantine::quarantine(&self.base, &list) {
                Ok(out) => {
                    let moved: Vec<PathBuf> = out.batch.entries.iter().map(|e| e.original.clone()).collect();
                    let (gone, stay): (Vec<Row>, Vec<Row>) = files.into_iter().partition(|r| moved.contains(&r.item.path));
                    self.rows.extend(stay.into_iter().map(|mut r| {
                        r.checked = false;
                        r
                    }));
                    if !gone.is_empty() {
                        msgs.push(format!("Moved {} items ({}) to quarantine. Press u to undo.", gone.len(), human(out.batch.bytes())));
                        self.last_batch = Some((out.batch.id.clone(), gone));
                    }
                    if !out.skipped.is_empty() {
                        msgs.push(format!("{} skipped.", out.skipped.len()));
                    }
                }
                Err(e) => {
                    msgs.push(format!("Quarantine failed: {e}"));
                    self.rows.extend(files);
                }
            }
        }

        if !dock.is_empty() {
            let refs: Vec<&Item> = dock.iter().map(|r| &r.item).collect();
            let mut ok = true;
            for args in docker::prune_commands(&refs) {
                if let Err(e) = docker::run(&args) {
                    msgs.push(format!("docker {} failed: {e}", args.join(" ")));
                    ok = false;
                }
            }
            if ok {
                let b: u64 = dock.iter().map(|r| r.item.bytes).sum();
                msgs.push(format!("Pruned {} of Docker data.", human(b)));
            } else {
                self.rows.extend(dock);
            }
        }
        self.rows.sort_by(|a, b| b.item.bytes.cmp(&a.item.bytes));
        self.move_by(0);
        self.status = msgs.join(" ");
    }

    pub fn undo(&mut self) {
        let Some((id, rows)) = self.last_batch.take() else {
            self.status = "Nothing to undo in this session. `sweepr undo` restores older batches.".into();
            return;
        };
        match quarantine::restore(&self.base, &id) {
            Ok(out) => {
                let n = out.restored.len();
                self.rows.extend(rows.into_iter().filter(|r| out.restored.contains(&r.item.path)).map(|mut r| {
                    r.checked = false;
                    r
                }));
                self.rows.sort_by(|a, b| b.item.bytes.cmp(&a.item.bytes));
                self.status = if out.failed.is_empty() {
                    format!("Restored {n} items.")
                } else {
                    format!("Restored {n}; {} kept in quarantine because something new is in their place.", out.failed.len())
                };
            }
            Err(e) => self.status = format!("Undo failed: {e}"),
        }
        self.move_by(0);
    }

    pub fn on_key(&mut self, code: KeyCode) {
        if self.mode == Mode::Confirm {
            match code {
                KeyCode::Char('y') | KeyCode::Enter => self.clean(),
                _ => {
                    self.mode = Mode::Browse;
                    self.status = "Cancelled.".into();
                }
            }
            return;
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => self.switch_tab(),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') => self.toggle_all(),
            KeyCode::Char('c') => self.ask_clean(),
            KeyCode::Char('u') => self.undo(),
            _ => {}
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let [head, tabs, body, detail, foot] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(5),
            Constraint::Length(2),
        ])
        .areas(f.area());

        let title = Line::from(vec![
            Span::styled(" Sweepr ", Style::new().bold().fg(Color::Black).bg(Color::Green)),
            Span::raw("  dev junk, explained"),
        ]);
        let summary = Line::from(vec![
            Span::styled(format!(" Safe {} ", human(self.bytes(Bucket::Safe))), Style::new().fg(Color::Green)),
            Span::raw("· "),
            Span::styled(format!("Needs you {} ", human(self.bytes(Bucket::NeedsYou))), Style::new().fg(Color::Yellow)),
            Span::raw("· "),
            Span::styled(format!("Ticked {}", human(self.selected_bytes())), Style::new().bold()),
        ]);
        f.render_widget(Paragraph::new(vec![title, summary]), head);

        let n_safe = self.rows.iter().filter(|r| r.item.bucket == Bucket::Safe).count();
        let n_need = self.rows.len() - n_safe;
        let t = Tabs::new(vec![format!("Safe to clean ({n_safe})"), format!("Needs you ({n_need})")])
            .select(self.tab_ix())
            .highlight_style(Style::new().bold().underlined());
        f.render_widget(t, tabs);

        let home = dirs::home_dir();
        let items: Vec<ListItem> = self
            .visible()
            .into_iter()
            .map(|i| {
                let r = &self.rows[i];
                let mark = if !Self::can_tick(r) { "[-]" } else if r.checked { "[x]" } else { "[ ]" };
                let path = shorten(&r.item.path, home.as_deref());
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{mark} ")),
                    Span::styled(format!("{:>10}  ", human(r.item.bytes)), Style::new().bold()),
                    Span::styled(format!("{:<24} ", r.item.kind.label()), Style::new().fg(Color::Cyan)),
                    Span::raw(path),
                ]))
            })
            .collect();
        let empty = items.is_empty();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let ix = self.tab_ix();
        if empty {
            let msg = if self.tab == Bucket::Safe { "Nothing safe to clean. 🎉" } else { "Nothing needs you." };
            f.render_widget(Paragraph::new(msg).block(Block::default().borders(Borders::ALL)), body);
        } else {
            f.render_stateful_widget(list, body, &mut self.lists[ix]);
        }

        let text = match self.current() {
            Some(i) => {
                let r = &self.rows[i];
                vec![
                    Line::from(r.note.clone()),
                    Line::from(Span::styled(format!("Comes back with: {}", r.item.kind.restore_hint()), Style::new().dim())),
                ]
            }
            None => vec![Line::from("")],
        };
        f.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL).title(" Note ")),
            detail,
        );

        let keys = Line::from(Span::styled(
            " ↑↓ move  space tick  a all  tab switch  c clean  u undo  q quit",
            Style::new().dim(),
        ));
        f.render_widget(Paragraph::new(vec![Line::from(format!(" {}", self.status)), keys]), foot);

        if self.mode == Mode::Confirm {
            let text = self.confirm_text().unwrap_or_default();
            let area = centered(f.area(), 64, 7);
            f.render_widget(Clear, area);
            let mut lines: Vec<Line> = text.lines().map(|l| Line::from(l.to_string())).collect();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("y / enter to confirm, any other key to cancel", Style::new().dim())));
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL).title(" Clean? ")),
                area,
            );
        }
    }
}

pub fn run(items: Vec<Item>) -> std::io::Result<()> {
    let mut app = App::new(items, quarantine::base_dir());
    let mut term: DefaultTerminal = ratatui::init();
    let res = (|| -> std::io::Result<()> {
        while !app.quit {
            term.draw(|f| app.draw(f))?;
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k.code);
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    res
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn shorten(p: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Kind;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;

    fn item(path: PathBuf, kind: Kind, bucket: Bucket, bytes: u64) -> Item {
        Item { path, kind, bucket, bytes, idle_days: Some(40), reason: "test".into() }
    }

    #[test]
    fn safe_ticked_needs_you_not_and_docker_needs_you_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            vec![
                item("/x/a/node_modules".into(), Kind::NodeModules, Bucket::Safe, 100),
                item("/x/big.iso".into(), Kind::LargeFile, Bucket::NeedsYou, 50),
                item("docker volume pg".into(), Kind::DockerVolume, Bucket::NeedsYou, 70),
            ],
            tmp.path().join("q"),
        );
        assert_eq!(app.selected_bytes(), 100);
        app.switch_tab();
        app.toggle_all();
        assert_eq!(app.selected_bytes(), 150, "large file ticks, docker volume can't");
        app.move_by(1);
        app.toggle();
        assert!(app.status.contains("never removes Docker volumes"));
    }

    #[test]
    fn clean_then_undo_from_the_tui() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let nm = proj.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("f"), "x").unwrap();
        let mut app = App::new(vec![item(nm.clone(), Kind::NodeModules, Bucket::Safe, 1)], tmp.path().join("q"));

        app.on_key(KeyCode::Char('c'));
        assert_eq!(app.mode, Mode::Confirm);
        app.on_key(KeyCode::Char('y'));
        assert!(!nm.exists());
        assert!(app.rows.is_empty());
        assert!(app.status.contains("Press u to undo"));

        app.on_key(KeyCode::Char('u'));
        assert!(nm.join("f").exists());
        assert_eq!(app.rows.len(), 1);
        assert!(!app.rows[0].checked, "restored rows come back unticked");
    }

    #[test]
    fn cancel_moves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp.path().join("p").join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        let mut app = App::new(vec![item(nm.clone(), Kind::NodeModules, Bucket::Safe, 1)], tmp.path().join("q"));
        app.on_key(KeyCode::Char('c'));
        app.on_key(KeyCode::Char('n'));
        assert!(nm.exists());
        assert_eq!(app.status, "Cancelled.");
    }

    #[test]
    fn renders_both_tabs_and_note() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            vec![item("/x/app/node_modules".into(), Kind::NodeModules, Bucket::Safe, 2048)],
            tmp.path().join("q"),
        );
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let screen: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        for s in ["Sweepr", "Safe to clean (1)", "Needs you (0)", "[x]", "node_modules for app", "c clean"] {
            assert!(screen.contains(s), "missing {s}");
        }
    }
}
