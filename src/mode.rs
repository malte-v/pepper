use crate::{
    buffer::{BufferCollection, TextRef},
    buffer_position::BufferOffset,
    buffer_view::BufferView,
    event::Key,
};

pub enum Transition {
    None,
    Waiting,
    Exit,
    EnterMode(Box<dyn Mode>),
}

pub trait Mode {
    fn on_event(
        &mut self,
        buffer_view: &mut BufferView,
        buffers: &mut BufferCollection,
        keys: &[Key],
    ) -> Transition;
}

pub fn initial_mode() -> Box<dyn Mode> {
    Box::new(Normal)
}

pub struct Normal;
impl Mode for Normal {
    fn on_event(
        &mut self,
        buffer_view: &mut BufferView,
        buffers: &mut BufferCollection,
        keys: &[Key],
    ) -> Transition {
        match keys {
            [Key::Char('q')] => return Transition::Waiting,
            [Key::Char('q'), Key::Char('q')] => return Transition::Exit,
            [Key::Char('h')] => buffer_view.move_cursor(
                buffers,
                BufferOffset {
                    column_offset: -1,
                    line_offset: 0,
                },
            ),
            [Key::Char('j')] => buffer_view.move_cursor(
                buffers,
                BufferOffset {
                    column_offset: 0,
                    line_offset: 1,
                },
            ),
            [Key::Char('k')] => buffer_view.move_cursor(
                buffers,
                BufferOffset {
                    column_offset: 0,
                    line_offset: -1,
                },
            ),
            [Key::Char('l')] => buffer_view.move_cursor(
                buffers,
                BufferOffset {
                    column_offset: 1,
                    line_offset: 0,
                },
            ),
            [Key::Char('i')] => return Transition::EnterMode(Box::new(Insert)),
            [Key::Char('u')] => buffer_view.undo(buffers),
            [Key::Char('U')] => buffer_view.redo(buffers),
            _ => (),
        }

        Transition::None
    }
}

struct Insert;
impl Mode for Insert {
    fn on_event(
        &mut self,
        buffer_view: &mut BufferView,
        buffers: &mut BufferCollection,
        keys: &[Key],
    ) -> Transition {
        match keys {
            [Key::Esc] | [Key::Ctrl('c')] => {
                buffer_view.commit_edits(buffers);
                return Transition::EnterMode(Box::new(Normal));
            }
            [Key::Tab] => {
                buffer_view.insert_text(buffers, TextRef::Str("    "));
            }
            [Key::Enter] | [Key::Ctrl('m')] => {
                buffer_view.insert_text(buffers, TextRef::Char('\n'));
            }
            [Key::Char(c)] => {
                buffer_view.insert_text(buffers, TextRef::Char(*c));
            }
            [Key::Delete] => {
                buffer_view.delete_selection(buffers);
            }
            _ => (),
        }

        Transition::None
    }
}
