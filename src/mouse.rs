//! Route terminal pointer reports to the items in the last painted frame.
use super::Controls;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use jscope::{
    state::State,
    ui::{MouseMap, MouseTarget},
};

const WHEEL_LINES: isize = 3;

pub(crate) enum Response {
    Ignored,
    Redraw,
    Quit,
}

// Request reporting from terminal emulators, including sessions inside tmux or SSH.
// Plain consoles and terminals that ignore the request remain keyboard-driven.
pub(crate) fn supports_capture(term: Option<&str>) -> bool {
    !matches!(term, None | Some("" | "dumb" | "linux" | "vt100" | "vt220"))
}

impl Controls {
    pub(crate) fn mouse(
        &mut self,
        event: MouseEvent,
        map: &MouseMap,
        state: &mut State,
    ) -> Response {
        // Ignore releases, drags and movement without even looking up a hit region.
        let kind = event.kind;
        if !matches!(
            kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ) {
            return Response::Ignored;
        }
        let Some(target) = map.target(event.column, event.row) else {
            return Response::Ignored;
        };
        if kind == MouseEventKind::Down(MouseButton::Right) {
            return self.mouse_back(state);
        }
        if let (MouseEventKind::Down(MouseButton::Left), MouseTarget::Shortcut(code)) =
            (kind, target)
        {
            return self.mouse_key(*code, state);
        }
        if self.previous_search.is_some() {
            return Response::Ignored;
        }
        self.notice = None;
        match kind {
            MouseEventKind::Down(MouseButton::Left) => self.click_target(target, state),
            MouseEventKind::ScrollUp => self.wheel(target, -1, state),
            MouseEventKind::ScrollDown => self.wheel(target, 1, state),
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                let step = if kind == MouseEventKind::ScrollLeft {
                    -1
                } else {
                    1
                };
                match target {
                    MouseTarget::Occurrence => state.move_occurrence(step),
                    MouseTarget::Spike | MouseTarget::Bin { .. } | MouseTarget::Rank(_) => {
                        state.move_spike(step)
                    }
                    _ => return Response::Ignored,
                }
                Response::Redraw
            }
            _ => Response::Ignored,
        }
    }

    fn click_target(&mut self, target: &MouseTarget, state: &mut State) -> Response {
        match target {
            MouseTarget::Live => {
                self.closure_diff = None;
                self.service_focus = false;
                state.jump_live();
            }
            MouseTarget::Tail | MouseTarget::Row(_) => {
                self.service_focus = false;
                state.cancel_spike();
                if let MouseTarget::Row(id) = target
                    && !state.toggle_row_details(*id)
                {
                    self.notice = Some("That entry is no longer visible".into());
                }
            }
            MouseTarget::Services | MouseTarget::Leader => self.service_focus = true,
            MouseTarget::Service(unit) => {
                self.service_focus = true;
                if !state.inspect_unit(unit) {
                    self.notice = Some("That service has left the activity window".into());
                }
            }
            MouseTarget::Bin { peer, index } => {
                if *peer {
                    state.switch_spike_unit();
                }
                if let Some(spike) = state.spike() {
                    state.move_spike(*index as isize - spike.selected as isize);
                }
            }
            MouseTarget::Rank(rank) => {
                self.notice = state.follow_message(*rank).err().map(String::from)
            }
            MouseTarget::ContextRow(index) => {
                if let Some(context) = state.context() {
                    state.move_context(*index as isize - context.selected as isize);
                }
            }
            _ => return Response::Ignored,
        }
        Response::Redraw
    }

    fn wheel(&mut self, target: &MouseTarget, step: isize, state: &mut State) -> Response {
        let lines = step * WHEEL_LINES;
        match target {
            MouseTarget::Tail | MouseTarget::Row(_) => {
                self.service_focus = false;
                state.cancel_spike();
                state.scroll_tail(lines);
            }
            MouseTarget::Services | MouseTarget::Service(_) | MouseTarget::Leader => {
                self.service_focus = true;
                state.move_unit(step);
            }
            MouseTarget::Spike | MouseTarget::Bin { .. } | MouseTarget::Rank(_) => {
                state.move_spike(step)
            }
            MouseTarget::Context | MouseTarget::ContextRow(_) => state.move_context(lines),
            MouseTarget::Occurrence if self.detail_scroll_limit == 0 => state.move_occurrence(step),
            MouseTarget::Occurrence | MouseTarget::Details => {
                state.scroll_details(lines, self.detail_scroll_limit)
            }
            MouseTarget::Closure => {
                if let Some(diff) = &mut self.closure_diff {
                    diff.scroll = diff
                        .scroll
                        .saturating_add_signed(lines)
                        .min(self.detail_scroll_limit);
                }
            }
            MouseTarget::Live | MouseTarget::Shortcut(_) => return Response::Ignored,
        }
        Response::Redraw
    }

    fn mouse_key(&mut self, code: KeyCode, state: &mut State) -> Response {
        if self.key(KeyEvent::from(code), state) {
            Response::Redraw
        } else {
            Response::Quit
        }
    }

    fn mouse_back(&mut self, state: &mut State) -> Response {
        if self.previous_search.is_some()
            || self.closure_diff.is_some()
            || state.context().is_some()
            || state.spike().is_some()
        {
            return self.mouse_key(KeyCode::Esc, state);
        }
        if state.expanded_group().is_some() {
            state.toggle_expanded();
        } else if self.service_focus {
            self.service_focus = false;
        } else if state.is_paused() {
            state.jump_live();
        } else {
            return Response::Ignored;
        }
        self.notice = None;
        Response::Redraw
    }
}
