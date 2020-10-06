use std::{fs::File, io::Read, path::Path};

use crate::{
    buffer::{Buffer, BufferCollection, BufferContent, BufferHandle},
    buffer_position::{BufferPosition, BufferRange},
    client::ClientCollection,
    client::TargetClient,
    cursor::{Cursor, CursorCollection},
    history::{Edit, EditKind},
    script::ScriptValue,
    syntax::SyntaxCollection,
    word_database::{WordDatabase, WordKind},
};

pub enum CursorMovement {
    ColumnsForward(usize),
    ColumnsBackward(usize),
    LinesForward(usize),
    LinesBackward(usize),
    WordsForward(usize),
    WordsBackward(usize),
    Home,
    End,
    FirstLine,
    LastLine,
}

#[derive(Clone, Copy)]
pub enum CursorMovementKind {
    PositionAndAnchor,
    PositionOnly,
}

pub struct BufferView {
    pub target_client: TargetClient,
    pub buffer_handle: BufferHandle,
    pub cursors: CursorCollection,
}

impl BufferView {
    pub fn new(target_client: TargetClient, buffer_handle: BufferHandle) -> Self {
        Self {
            target_client,
            buffer_handle,
            cursors: CursorCollection::new(),
        }
    }

    pub fn clone_with_target_client(&self, target_client: TargetClient) -> Self {
        Self {
            target_client,
            buffer_handle: self.buffer_handle,
            cursors: self.cursors.clone(),
        }
    }

    pub fn move_cursors(
        &mut self,
        buffers: &BufferCollection,
        movement: CursorMovement,
        movement_kind: CursorMovementKind,
    ) {
        fn try_nth<I, E>(iter: I, mut n: usize) -> Result<E, usize>
        where
            I: Iterator<Item = E>,
        {
            for e in iter {
                if n == 0 {
                    return Ok(e);
                }
                n -= 1;
            }
            Err(n)
        }

        let buffer = match buffers.get(self.buffer_handle) {
            Some(buffer) => &buffer.content,
            None => return,
        };

        let mut cursors = self.cursors.mut_guard();
        match movement {
            CursorMovement::ColumnsForward(n) => {
                let last_line_index = buffer.line_count() - 1;
                for c in &mut cursors[..] {
                    let line = buffer.line_at(c.position.line_index).as_str();
                    match try_nth(line[c.position.column_byte_index..].char_indices(), n) {
                        Ok((i, _)) => c.position.column_byte_index += i,
                        Err(0) => c.position.column_byte_index = line.len(),
                        Err(mut n) => {
                            n -= 1;
                            loop {
                                if c.position.line_index == last_line_index {
                                    c.position.column_byte_index =
                                        buffer.line_at(last_line_index).as_str().len();
                                    break;
                                }

                                c.position.line_index += 1;
                                let line = buffer.line_at(c.position.line_index).as_str();
                                match try_nth(line.char_indices(), n) {
                                    Ok((i, _)) => {
                                        c.position.column_byte_index = i;
                                        break;
                                    }
                                    Err(0) => {
                                        c.position.column_byte_index = line.len();
                                        break;
                                    }
                                    Err(rest) => n = rest - 1,
                                }
                            }
                        }
                    }
                }
            }
            CursorMovement::ColumnsBackward(n) => {
                if n == 0 {
                    return;
                }
                let n = n - 1;

                for c in &mut cursors[..] {
                    let line = buffer.line_at(c.position.line_index).as_str();
                    match try_nth(line[..c.position.column_byte_index].char_indices().rev(), n) {
                        Ok((i, _)) => c.position.column_byte_index = i,
                        Err(0) => {
                            if c.position.line_index == 0 {
                                c.position.column_byte_index = 0;
                            } else {
                                c.position.line_index -= 1;
                                c.position.column_byte_index =
                                    buffer.line_at(c.position.line_index).as_str().len();
                            }
                        }
                        Err(mut n) => {
                            n -= 1;
                            loop {
                                if c.position.line_index == 0 {
                                    c.position.column_byte_index = 0;
                                    break;
                                }

                                c.position.line_index -= 1;
                                let line = buffer.line_at(c.position.line_index).as_str();
                                match try_nth(line.char_indices().rev(), n) {
                                    Ok((i, _)) => {
                                        c.position.column_byte_index = i;
                                        break;
                                    }
                                    Err(0) => {
                                        if c.position.line_index == 0 {
                                            c.position.column_byte_index = 0;
                                        } else {
                                            c.position.line_index -= 1;
                                            c.position.column_byte_index = buffer
                                                .line_at(c.position.line_index)
                                                .as_str()
                                                .len();
                                        }
                                        break;
                                    }
                                    Err(rest) => n = rest - 1,
                                }
                            }
                        }
                    }
                }
            }
            CursorMovement::LinesForward(n) => {
                for c in &mut cursors[..] {
                    c.position.line_index = buffer
                        .line_count()
                        .saturating_sub(1)
                        .min(c.position.line_index + n);
                    c.position = buffer.saturate_position(c.position);
                }
            }
            CursorMovement::LinesBackward(n) => {
                for c in &mut cursors[..] {
                    c.position.line_index = c.position.line_index.saturating_sub(n);
                    c.position = buffer.saturate_position(c.position);
                }
            }
            CursorMovement::WordsForward(n) => {
                if n == 0 {
                    return;
                }

                let last_line_index = buffer.line_count() - 1;
                for c in &mut cursors[..] {
                    let mut n = n;

                    loop {
                        let (word, _, right_words) = buffer.words_from(c.position);
                        if word.kind != WordKind::Whitespace {
                            if n == 0 {
                                c.position = word.position;
                                break;
                            }
                            n -= 1;
                        }

                        match try_nth(right_words.filter(|w| w.kind != WordKind::Whitespace), n) {
                            Ok(word) => {
                                c.position = word.position;
                                break;
                            }
                            Err(0) => {
                                c.position.column_byte_index =
                                    buffer.line_at(c.position.line_index).as_str().len();
                                break;
                            }
                            Err(rest) => {
                                if c.position.line_index == last_line_index {
                                    c.position.column_byte_index =
                                        buffer.line_at(last_line_index).as_str().len();
                                    break;
                                }

                                n = rest - 1;
                                c.position.line_index += 1;
                                c.position.column_byte_index = 0;
                            }
                        }
                    }
                }
            }
            CursorMovement::WordsBackward(n) => {
                if n == 0 {
                    return;
                }
                let n = n - 1;

                for c in &mut cursors[..] {
                    let mut n = n;

                    loop {
                        let (word, left_words, _) = buffer.words_from(c.position);
                        if word.kind != WordKind::Whitespace
                            && c.position.column_byte_index != word.position.column_byte_index
                        {
                            if n == 0 {
                                c.position = word.position;
                                break;
                            }
                            n -= 1;
                        }

                        match try_nth(left_words.filter(|w| w.kind != WordKind::Whitespace), n) {
                            Ok(word) => {
                                c.position = word.position;
                                break;
                            }
                            Err(0) => {
                                if c.position.line_index > 0 {
                                    c.position.line_index -= 1;
                                    c.position.column_byte_index =
                                        buffer.line_at(c.position.line_index).as_str().len()
                                }
                                break;
                            }
                            Err(rest) => {
                                if c.position.line_index == 0 {
                                    c.position.column_byte_index = 0;
                                    break;
                                }

                                n = rest - 1;
                                c.position.line_index -= 1;
                                c.position.column_byte_index =
                                    buffer.line_at(c.position.line_index).as_str().len();
                            }
                        }
                    }
                }
            }
            CursorMovement::Home => {
                for c in &mut cursors[..] {
                    c.position.column_byte_index = 0;
                }
            }
            CursorMovement::End => {
                for c in &mut cursors[..] {
                    c.position.column_byte_index =
                        buffer.line_at(c.position.line_index).as_str().len();
                }
            }
            CursorMovement::FirstLine => {
                for c in &mut cursors[..] {
                    c.position.line_index = 0;
                    c.position = buffer.saturate_position(c.position);
                }
            }
            CursorMovement::LastLine => {
                for c in &mut cursors[..] {
                    c.position.line_index = buffer.line_count() - 1;
                    c.position = buffer.saturate_position(c.position);
                }
            }
        }

        if let CursorMovementKind::PositionAndAnchor = movement_kind {
            for c in &mut cursors[..] {
                c.anchor = c.position;
            }
        }
    }

    pub fn commit_edits(&self, buffers: &mut BufferCollection) {
        if let Some(buffer) = buffers.get_mut(self.buffer_handle) {
            buffer.commit_edits();
        }
    }

    pub fn get_selection_text(&self, buffers: &BufferCollection, text: &mut String) {
        text.clear();

        let buffer = match buffers.get(self.buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };

        let mut iter = self.cursors[..].iter();
        if let Some(cursor) = iter.next() {
            let mut last_range = cursor.as_range();
            buffer.content.append_range_text_to_string(last_range, text);
            for cursor in iter {
                let range = cursor.as_range();
                if range.from.line_index > last_range.to.line_index {
                    text.push('\n');
                }
                buffer.content.append_range_text_to_string(range, text);
                last_range = range;
            }
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct BufferViewHandle(usize);

impl_from_script!(BufferViewHandle, from => match from {
    ScriptValue::Integer(n) if n >= 0 => Some(Self(n as _)),
    _ => None,
});
impl_to_script!(BufferViewHandle, self => ScriptValue::Integer(self.0 as _));

#[derive(Default)]
pub struct BufferViewCollection {
    buffer_views: Vec<Option<BufferView>>,
    fix_cursor_ranges: Vec<BufferRange>,
}

impl BufferViewCollection {
    pub fn add(&mut self, buffer_view: BufferView) -> BufferViewHandle {
        for (i, slot) in self.buffer_views.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(buffer_view);
                return BufferViewHandle(i);
            }
        }

        let handle = BufferViewHandle(self.buffer_views.len());
        self.buffer_views.push(Some(buffer_view));
        handle
    }

    pub fn remove_where<F>(
        &mut self,
        buffers: &mut BufferCollection,
        clients: &mut ClientCollection,
        word_database: &mut WordDatabase,
        predicate: F,
    ) where
        F: Fn(&BufferView) -> bool,
    {
        for i in 0..self.buffer_views.len() {
            if let Some(view) = &self.buffer_views[i] {
                if predicate(&view) {
                    self.buffer_views[i] = None;
                }
            }
        }

        buffers.remove_where(clients, word_database, |h, _| {
            !self.iter().any(|v| v.buffer_handle == h)
        });
    }

    pub fn get(&self, handle: BufferViewHandle) -> Option<&BufferView> {
        self.buffer_views[handle.0].as_ref()
    }

    pub fn get_mut(&mut self, handle: BufferViewHandle) -> Option<&mut BufferView> {
        self.buffer_views[handle.0].as_mut()
    }

    pub fn iter(&self) -> impl Iterator<Item = &BufferView> {
        self.buffer_views.iter().flatten()
    }

    fn iter_with_handles(&self) -> impl Iterator<Item = (BufferViewHandle, &BufferView)> {
        self.buffer_views
            .iter()
            .enumerate()
            .filter_map(|(i, v)| Some(BufferViewHandle(i)).zip(v.as_ref()))
    }

    pub fn insert_text_at_position(
        &mut self,
        buffers: &mut BufferCollection,
        word_database: &mut WordDatabase,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
        position: BufferPosition,
        text: &str,
        cursor_index: usize,
    ) {
        let current_view = match &mut self.buffer_views[handle.0] {
            Some(view) => view,
            None => return,
        };
        let buffer = match buffers.get_mut(current_view.buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };

        self.fix_cursor_ranges.clear();
        let range = buffer.insert_text(word_database, syntaxes, position, text, cursor_index);
        self.fix_cursor_ranges.push(range);

        let current_buffer_handle = current_view.buffer_handle;
        self.fix_buffer_cursors(current_buffer_handle, |cursor, range| cursor.insert(range));
    }

    pub fn insert_text_at_cursor_positions(
        &mut self,
        buffers: &mut BufferCollection,
        word_database: &mut WordDatabase,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
        text: &str,
    ) {
        let current_view = match &mut self.buffer_views[handle.0] {
            Some(view) => view,
            None => return,
        };
        let buffer = match buffers.get_mut(current_view.buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };

        self.fix_cursor_ranges.clear();
        for (i, cursor) in current_view.cursors[..].iter().enumerate().rev() {
            let range = buffer.insert_text(word_database, syntaxes, cursor.position, text, i);
            self.fix_cursor_ranges.push(range);
        }

        let current_buffer_handle = current_view.buffer_handle;
        self.fix_buffer_cursors(current_buffer_handle, |cursor, range| cursor.insert(range));
    }

    pub fn delete_in_range(
        &mut self,
        buffers: &mut BufferCollection,
        word_database: &mut WordDatabase,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
        range: BufferRange,
        cursor_index: usize,
    ) {
        let current_view = match &mut self.buffer_views[handle.0] {
            Some(view) => view,
            None => return,
        };
        let buffer = match buffers.get_mut(current_view.buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };

        self.fix_cursor_ranges.clear();
        self.fix_cursor_ranges.push(range);
        buffer.delete_range(word_database, syntaxes, range, cursor_index);

        let current_buffer_handle = current_view.buffer_handle;
        self.fix_buffer_cursors(current_buffer_handle, |cursor, range| cursor.delete(range));
    }

    pub fn delete_in_cursor_ranges(
        &mut self,
        buffers: &mut BufferCollection,
        word_database: &mut WordDatabase,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
    ) {
        let current_view = match &mut self.buffer_views[handle.0] {
            Some(view) => view,
            None => return,
        };
        let buffer = match buffers.get_mut(current_view.buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };

        self.fix_cursor_ranges.clear();
        for (i, cursor) in current_view.cursors[..].iter().enumerate().rev() {
            let range = cursor.as_range();
            buffer.delete_range(word_database, syntaxes, range, i);
            self.fix_cursor_ranges.push(range);
        }

        let current_buffer_handle = current_view.buffer_handle;
        self.fix_buffer_cursors(current_buffer_handle, |cursor, range| cursor.delete(range));
    }

    pub fn apply_completion(
        &mut self,
        buffers: &mut BufferCollection,
        word_database: &mut WordDatabase,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
        completion: &str,
    ) {
        let current_view = match &mut self.buffer_views[handle.0] {
            Some(view) => view,
            None => return,
        };
        let buffer = match buffers.get_mut(current_view.buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };

        self.fix_cursor_ranges.clear();
        for (i, cursor) in current_view.cursors[..].iter().enumerate().rev() {
            let mut word_position = cursor.position;
            word_position.column_byte_index = word_position.column_byte_index.saturating_sub(1);
            let word = buffer.content.word_at(word_position);
            let word_kind = word.kind;
            let word_position = word.position;

            if let WordKind::Identifier = word_kind {
                let range = BufferRange::between(word_position, cursor.position);
                buffer.delete_range(word_database, syntaxes, range, i);
            }

            let insert_range =
                buffer.insert_text(word_database, syntaxes, word_position, completion, i);
            let mut range = BufferRange::between(cursor.position, insert_range.to);
            if cursor.position > insert_range.to {
                std::mem::swap(&mut range.from, &mut range.to);
            }
            self.fix_cursor_ranges.push(range);
        }

        let current_buffer_handle = current_view.buffer_handle;
        self.fix_buffer_cursors(current_buffer_handle, |cursor, mut range| {
            if range.from <= range.to {
                cursor.insert(range);
            } else {
                std::mem::swap(&mut range.from, &mut range.to);
                cursor.delete(range);
            }
        });
    }

    fn fix_buffer_cursors(
        &mut self,
        buffer_handle: BufferHandle,
        op: fn(&mut Cursor, BufferRange),
    ) {
        for view in self.buffer_views.iter_mut().flatten() {
            if view.buffer_handle != buffer_handle {
                continue;
            }

            let ranges = &self.fix_cursor_ranges;
            for c in &mut view.cursors.mut_guard()[..] {
                for range in ranges.iter() {
                    op(c, *range);
                }
            }
        }
    }

    pub fn undo(
        &mut self,
        buffers: &mut BufferCollection,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
    ) {
        if let Some(buffer) = self.buffer_views[handle.0]
            .as_mut()
            .and_then(|view| buffers.get_mut(view.buffer_handle))
        {
            self.apply_edits(handle, buffer.undo(syntaxes));
        }
    }

    pub fn redo(
        &mut self,
        buffers: &mut BufferCollection,
        syntaxes: &SyntaxCollection,
        handle: BufferViewHandle,
    ) {
        if let Some(buffer) = self.buffer_views[handle.0]
            .as_mut()
            .and_then(|view| buffers.get_mut(view.buffer_handle))
        {
            self.apply_edits(handle, buffer.redo(syntaxes));
        }
    }

    fn apply_edits<'a>(
        &mut self,
        handle: BufferViewHandle,
        edits: impl 'a + Iterator<Item = Edit<'a>>,
    ) {
        let buffer_handle = match self.get(handle) {
            Some(view) => view.buffer_handle,
            None => return,
        };

        self.fix_cursor_ranges.clear();
        for edit in edits {
            let cursor_index = edit.cursor_index as usize;
            if cursor_index >= self.fix_cursor_ranges.len() {
                self.fix_cursor_ranges
                    .resize(cursor_index + 1, BufferRange::default());
            }

            match edit.kind {
                EditKind::Insert => {
                    self.fix_cursor_ranges[cursor_index].from = edit.range.to;
                    for (i, view) in self.buffer_views.iter_mut().flatten().enumerate() {
                        if i != handle.0 && view.buffer_handle == buffer_handle {
                            for c in &mut view.cursors.mut_guard()[..] {
                                c.insert(edit.range);
                            }
                        }
                    }
                }
                EditKind::Delete => {
                    self.fix_cursor_ranges[cursor_index].from = edit.range.from;
                    for (i, view) in self.buffer_views.iter_mut().flatten().enumerate() {
                        if i != handle.0 && view.buffer_handle == buffer_handle {
                            for c in &mut view.cursors.mut_guard()[..] {
                                c.delete(edit.range);
                            }
                        }
                    }
                }
            }
        }

        if let Some(view) = self.buffer_views[handle.0].as_mut() {
            if !self.fix_cursor_ranges.is_empty() {
                let mut cursors = view.cursors.mut_guard();
                cursors.clear();
                for range in &self.fix_cursor_ranges {
                    cursors.add(Cursor {
                        anchor: range.from,
                        position: range.from,
                    });
                }
            }
        }
    }

    pub fn buffer_view_handle_from_buffer_handle(
        &mut self,
        target_client: TargetClient,
        buffer_handle: BufferHandle,
    ) -> BufferViewHandle {
        let current_buffer_view_handle = self
            .iter_with_handles()
            .filter(|(_, view)| {
                view.buffer_handle == buffer_handle && view.target_client == target_client
            })
            .map(|(h, _)| h)
            .next();

        match current_buffer_view_handle {
            Some(handle) => handle,
            None => self.add(BufferView::new(target_client, buffer_handle)),
        }
    }

    pub fn buffer_view_handle_from_path(
        &mut self,
        buffers: &mut BufferCollection,
        word_database: &mut WordDatabase,
        syntaxes: &SyntaxCollection,
        target_client: TargetClient,
        path: &Path,
    ) -> Result<BufferViewHandle, String> {
        if let Some(buffer_handle) = buffers.find_with_path(path) {
            Ok(self.buffer_view_handle_from_buffer_handle(target_client, buffer_handle))
        } else if path.to_str().map(|s| s.trim().len()).unwrap_or(0) > 0 {
            let content = match File::open(&path) {
                Ok(mut file) => {
                    let mut content = String::new();
                    match file.read_to_string(&mut content) {
                        Ok(_) => (),
                        Err(error) => {
                            return Err(format!(
                                "could not read contents from file {:?}: {:?}",
                                path, error
                            ))
                        }
                    }
                    BufferContent::from_str(&content)
                }
                Err(_) => BufferContent::from_str(""),
            };

            let buffer_handle = buffers.add(Buffer::new(
                word_database,
                syntaxes,
                Some(path.into()),
                content,
            ));
            let buffer_view = BufferView::new(target_client, buffer_handle);
            let buffer_view_handle = self.add(buffer_view);
            Ok(buffer_view_handle)
        } else {
            Err(format!("invalid path {:?}", path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestContext {
        pub word_database: WordDatabase,
        pub syntaxes: SyntaxCollection,
        pub buffers: BufferCollection,
        pub buffer_views: BufferViewCollection,
        pub buffer_view_handle: BufferViewHandle,
    }

    impl TestContext {
        pub fn with_buffer(text: &str) -> Self {
            let mut word_database = WordDatabase::new();
            let syntaxes = SyntaxCollection::new();

            let mut buffers = BufferCollection::default();
            let buffer_content = BufferContent::from_str(text);
            let buffer_handle = buffers.add(Buffer::new(
                &mut word_database,
                &syntaxes,
                None,
                buffer_content,
            ));

            let buffer_view = BufferView::new(TargetClient::Local, buffer_handle);

            let mut buffer_views = BufferViewCollection::default();
            let buffer_view_handle = buffer_views.add(buffer_view);

            Self {
                word_database,
                syntaxes,
                buffers,
                buffer_views,
                buffer_view_handle,
            }
        }
    }

    #[test]
    fn buffer_view_utf8_support() {
        let mut ctx = TestContext::with_buffer("");

        let buffer_view = ctx.buffer_views.get(ctx.buffer_view_handle).unwrap();
        let main_cursor = buffer_view.cursors.main_cursor();
        assert_eq!(BufferPosition::line_col(0, 0), main_cursor.anchor);
        assert_eq!(BufferPosition::line_col(0, 0), main_cursor.position);

        ctx.buffer_views.insert_text_at_cursor_positions(
            &mut ctx.buffers,
            &mut ctx.word_database,
            &ctx.syntaxes,
            ctx.buffer_view_handle,
            "ç",
        );

        let buffer_view = ctx.buffer_views.get(ctx.buffer_view_handle).unwrap();
        let main_cursor = buffer_view.cursors.main_cursor();
        assert_eq!(BufferPosition::line_col(0, 2), main_cursor.anchor);
        assert_eq!(BufferPosition::line_col(0, 2), main_cursor.position);
    }

    #[test]
    fn buffer_view_cursor_movement() {
        fn set_cursor(ctx: &mut TestContext, position: BufferPosition) {
            let buffer_view = ctx.buffer_views.get_mut(ctx.buffer_view_handle).unwrap();
            let mut cursors = buffer_view.cursors.mut_guard();
            cursors.clear();
            cursors.add(Cursor {
                anchor: position,
                position,
            });
        }

        fn main_cursor_position(ctx: &TestContext) -> BufferPosition {
            ctx.buffer_views
                .get(ctx.buffer_view_handle)
                .unwrap()
                .cursors
                .main_cursor()
                .position
        }

        let mut ctx = TestContext::with_buffer("ab\nc e\nefgh\ni k\nlm");

        macro_rules! assert_movement {
            (($from_line:expr, $from_col:expr) => $movement:expr => ($to_line:expr, $to_col:expr)) => {
                set_cursor(&mut ctx, BufferPosition::line_col($from_line, $from_col));
                ctx.buffer_views
                    .get_mut(ctx.buffer_view_handle)
                    .unwrap()
                    .move_cursors(
                        &ctx.buffers,
                        $movement,
                        CursorMovementKind::PositionAndAnchor,
                    );
                assert_eq!(
                    BufferPosition::line_col($to_line, $to_col),
                    main_cursor_position(&ctx)
                );
            };
        }

        assert_movement!((2, 2) => CursorMovement::ColumnsForward(0) => (2, 2));
        assert_movement!((2, 2) => CursorMovement::ColumnsForward(1) => (2, 3));
        assert_movement!((2, 2) => CursorMovement::ColumnsForward(2) => (2, 4));
        assert_movement!((2, 2) => CursorMovement::ColumnsForward(3) => (3, 0));
        assert_movement!((2, 2) => CursorMovement::ColumnsForward(6) => (3, 3));
        assert_movement!((2, 2) => CursorMovement::ColumnsForward(7) => (4, 0));
        assert_movement!((2, 2) => CursorMovement::ColumnsForward(999) => (4, 2));

        assert_movement!((2, 2) => CursorMovement::ColumnsBackward(0) => (2, 2));
        assert_movement!((2, 2) => CursorMovement::ColumnsBackward(1) => (2, 1));
        assert_movement!((2, 0) => CursorMovement::ColumnsBackward(1) => (1, 3));
        assert_movement!((2, 2) => CursorMovement::ColumnsBackward(3) => (1, 3));
        assert_movement!((2, 2) => CursorMovement::ColumnsBackward(7) => (0, 2));
        assert_movement!((2, 2) => CursorMovement::ColumnsBackward(999) => (0, 0));

        assert_movement!((2, 2) => CursorMovement::WordsForward(0) => (2, 2));
        assert_movement!((2, 0) => CursorMovement::WordsForward(1) => (2, 4));
        assert_movement!((2, 0) => CursorMovement::WordsForward(2) => (3, 0));
        assert_movement!((2, 2) => CursorMovement::WordsForward(3) => (3, 2));
        assert_movement!((2, 2) => CursorMovement::WordsForward(4) => (3, 3));
        assert_movement!((2, 2) => CursorMovement::WordsForward(5) => (4, 0));
        assert_movement!((2, 2) => CursorMovement::WordsForward(6) => (4, 2));
        assert_movement!((2, 2) => CursorMovement::WordsForward(999) => (4, 2));

        assert_movement!((2, 2) => CursorMovement::WordsBackward(0) => (2, 2));
        assert_movement!((2, 0) => CursorMovement::WordsBackward(1) => (1, 3));
        assert_movement!((2, 0) => CursorMovement::WordsBackward(2) => (1, 2));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(1) => (2, 0));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(2) => (1, 3));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(3) => (1, 2));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(4) => (1, 0));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(5) => (0, 2));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(6) => (0, 0));
        assert_movement!((2, 2) => CursorMovement::WordsBackward(999) => (0, 0));
    }
}
