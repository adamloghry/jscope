//! Mouse targets from the most recently drawn frame.
use crossterm::event::KeyCode;
use ratatui::layout::{Position, Rect};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseTarget {
    Tail,
    Row(u64),
    Services,
    Service(String),
    Leader,
    Details,
    Context,
    ContextRow(usize),
    Spike,
    Bin { peer: bool, index: usize },
    Rank(usize),
    Occurrence,
    Closure,
    Live,
    Shortcut(KeyCode),
}

#[derive(Debug, Default)]
pub struct MouseMap {
    regions: Vec<(Rect, MouseTarget)>,
}

impl MouseMap {
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn target(&self, column: u16, row: u16) -> Option<&MouseTarget> {
        let position = Position::new(column, row);
        self.regions
            .iter()
            .rev()
            .find(|(area, _)| area.contains(position))
            .map(|(_, target)| target)
    }

    pub(super) fn add(&mut self, area: Rect, target: MouseTarget) {
        if !area.is_empty() {
            self.regions.push((area, target));
        }
    }

    pub(super) fn span(&mut self, row: Rect, start: usize, width: usize, target: MouseTarget) {
        if start < usize::from(row.width) {
            let start = start as u16;
            let width = width.min(usize::from(row.width - start)) as u16;
            self.add(
                Rect::new(row.x + start, row.y, width, row.height.min(1)),
                target,
            );
        }
    }

    pub(super) fn shortcut(&mut self, row: Rect, start: usize, key: &str, width: usize) {
        use KeyCode::{Char, Down, Enter, Esc, Left, PageDown, PageUp, Right, Tab, Up};

        // A combined hint only has a definite action on the individual key glyphs.
        let parts: &[(usize, usize, KeyCode)] = match key {
            "↑↓" => &[(0, 1, Up), (1, 1, Down)],
            "←→" => &[(0, 1, Left), (1, 1, Right)],
            "[]" => &[(0, 1, Char('[')), (1, 1, Char(']'))],
            "m/M" => &[(0, 1, Char('m')), (2, 1, Char('M'))],
            "PgUp/Dn" => &[(0, 4, PageUp), (5, 2, PageDown)],
            _ => {
                let code = match key {
                    "Tab" => Tab,
                    "↵" | "Enter" => Enter,
                    "Esc" => Esc,
                    "Space" => Char(' '),
                    single if single.len() == 1 => Char(char::from(single.as_bytes()[0])),
                    _ => return,
                };
                self.span(row, start, width, MouseTarget::Shortcut(code));
                return;
            }
        };
        for &(offset, len, code) in parts {
            self.span(row, start + offset, len, MouseTarget::Shortcut(code));
        }
    }
}
