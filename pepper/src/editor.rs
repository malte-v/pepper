use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::{
    buffer::{BufferCollection, BufferProperties, BufferReadError},
    buffer_position::{BufferPosition, BufferRange},
    buffer_view::{BufferViewCollection, BufferViewHandle},
    client::{ClientHandle, ClientManager},
    command::CommandManager,
    config::Config,
    editor_utils::{
        KeyMapCollection, MatchResult, ReadLine, RegisterCollection, RegisterKey, StatusBar,
        StringPool,
    },
    events::{
        ClientEvent, EditorEvent, EditorEventIter, EditorEventQueue, KeyParseAllError, KeyParser,
        ServerEvent, TargetClient,
    },
    mode::{Mode, ModeKind},
    pattern::Pattern,
    picker::Picker,
    platform::{Key, Platform, PlatformRequest},
    plugin::{PluginCollection, PluginHandle},
    syntax::{HighlightResult, SyntaxCollection},
    theme::Theme,
    ui,
    word_database::WordDatabase,
};

#[derive(Clone, Copy)]
pub enum EditorFlow {
    Continue,
    Suspend,
    Quit,
    QuitAll,
}

pub struct KeysIterator {
    pub index: usize,
}
impl KeysIterator {
    pub fn next(&mut self, keys: &BufferedKeys) -> Key {
        if self.index < keys.0.len() {
            let next = keys.0[self.index];
            self.index += 1;
            next
        } else {
            Key::None
        }
    }
}

pub struct BufferedKeysParseError<'a> {
    pub keys: &'a str,
    pub error: KeyParseAllError,
}
impl<'a> fmt::Display for BufferedKeysParseError<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "error parsing keys '{}'\n{}", self.keys, self.error)
    }
}

#[derive(Default)]
pub struct BufferedKeys(Vec<Key>);
impl BufferedKeys {
    pub fn as_slice(&self) -> &[Key] {
        &self.0
    }

    pub fn parse<'a>(&mut self, keys: &'a str) -> Result<KeysIterator, BufferedKeysParseError<'a>> {
        let index = self.as_slice().len();
        for key in KeyParser::new(keys) {
            match key {
                Ok(key) => self.0.push(key),
                Err(error) => {
                    self.0.truncate(index);
                    return Err(BufferedKeysParseError { keys, error });
                }
            }
        }

        Ok(KeysIterator { index })
    }
}

pub struct EditorContext {
    pub editor: Editor,
    pub platform: Platform,
    pub clients: ClientManager,
    pub plugins: PluginCollection,
}
impl EditorContext {
    pub(crate) fn render(&mut self) {
        let picker_height = self
            .editor
            .picker
            .update_scroll(self.editor.config.picker_max_height as _);
        let focused_client = self.clients.focused_client();

        let mut needs_redraw = false;
        for c in self.clients.iter_mut() {
            if let Some(handle) = c.buffer_view_handle() {
                let buffer_view = self.editor.buffer_views.get(handle);
                let buffer = self.editor.buffers.get_mut(buffer_view.buffer_handle);
                if let HighlightResult::Pending = buffer.update_highlighting(&self.editor.syntaxes)
                {
                    needs_redraw = true;
                }
            }

            let has_focus = focused_client == Some(c.handle());
            c.scroll_to_main_cursor(&self.editor, if has_focus { picker_height } else { 0 });

            if !c.has_ui() {
                continue;
            }

            let mut buf = self.platform.buf_pool.acquire();
            let write = buf.write_with_len(ServerEvent::bytes_variant_header_len());
            let ctx = ui::RenderContext {
                editor: &self.editor,
                viewport_size: c.viewport_size,
                scroll: c.scroll,
                has_focus,
            };
            ui::render(&ctx, c.buffer_view_handle(), write);
            ServerEvent::Display(&[]).serialize_bytes_variant_header(write);

            let handle = c.handle();
            self.platform
                .requests
                .enqueue(PlatformRequest::WriteToClient { handle, buf });
        }

        if needs_redraw {
            self.platform.requests.enqueue(PlatformRequest::Redraw);
        }
    }

    pub fn trigger_event_handlers(&mut self) {
        loop {
            self.editor.events.flip();
            let mut events = EditorEventIter::new();
            if events.next(&self.editor.events).is_none() {
                return;
            }

            PluginCollection::on_editor_events(self);

            let mut events = EditorEventIter::new();
            while let Some(event) = events.next(&self.editor.events) {
                match *event {
                    EditorEvent::Idle => (),
                    EditorEvent::BufferRead { handle } => {
                        let buffer = self.editor.buffers.get_mut(handle);
                        buffer.refresh_syntax(&self.editor.syntaxes);
                        self.editor.buffer_views.on_buffer_read(buffer);
                    }
                    EditorEvent::BufferInsertText { handle, range, .. } => {
                        self.editor
                            .buffer_views
                            .on_buffer_insert_text(handle, range);
                    }
                    EditorEvent::BufferDeleteText { handle, range } => {
                        self.editor
                            .buffer_views
                            .on_buffer_delete_text(handle, range);
                    }
                    EditorEvent::BufferWrite { handle, new_path } => {
                        let buffer = self.editor.buffers.get_mut(handle);
                        if new_path {
                            buffer.refresh_syntax(&self.editor.syntaxes);
                        }

                        for client in self.clients.iter() {
                            if client.stdin_buffer_handle() == Some(buffer.handle()) {
                                let mut buf = self.platform.buf_pool.acquire();
                                let write =
                                    buf.write_with_len(ServerEvent::bytes_variant_header_len());
                                let content = buffer.content();
                                let range =
                                    BufferRange::between(BufferPosition::zero(), content.end());
                                for text in content.text_range(range) {
                                    write.extend_from_slice(text.as_bytes());
                                }
                                ServerEvent::StdoutOutput(&[])
                                    .serialize_bytes_variant_header(write);

                                let handle = client.handle();
                                self.platform
                                    .requests
                                    .enqueue(PlatformRequest::WriteToClient { handle, buf });
                                break;
                            }
                        }
                    }
                    EditorEvent::BufferClose { handle } => {
                        self.editor.buffers.remove_now(
                            &mut self.platform,
                            handle,
                            &mut self.editor.word_database,
                        );
                        for client in self.clients.iter_mut() {
                            client.on_buffer_close(&mut self.editor, handle);
                        }
                        self.editor.buffer_views.remove_buffer_views(handle);
                        self.editor.mode.read_line_state.on_buffer_close(handle);
                    }
                    EditorEvent::FixCursors { handle, cursors } => {
                        let mut view_cursors =
                            self.editor.buffer_views.get_mut(handle).cursors.mut_guard();
                        view_cursors.clear();
                        for &cursor in cursors.as_cursors(&self.editor.events) {
                            view_cursors.add(cursor);
                        }
                    }
                }
            }
        }
    }
}

pub struct Editor {
    pub current_directory: PathBuf,
    pub config: Config,
    pub theme: Theme,
    pub syntaxes: SyntaxCollection,
    pub keymaps: KeyMapCollection,

    pub mode: Mode,
    pub buffers: BufferCollection,
    pub buffer_views: BufferViewCollection,
    pub word_database: WordDatabase,

    pub buffered_keys: BufferedKeys,
    pub recording_macro: Option<RegisterKey>,
    pub registers: RegisterCollection,
    pub read_line: ReadLine,
    pub picker: Picker,
    pub string_pool: StringPool,

    pub status_bar: StatusBar,
    pub aux_pattern: Pattern,

    pub commands: CommandManager,
    pub events: EditorEventQueue,
}
impl Editor {
    pub fn new(current_directory: PathBuf) -> Self {
        Self {
            current_directory,
            config: Config::default(),
            theme: Theme::default(),
            syntaxes: SyntaxCollection::new(),
            keymaps: KeyMapCollection::default(),

            mode: Mode::default(),

            buffers: BufferCollection::default(),
            buffer_views: BufferViewCollection::default(),
            word_database: WordDatabase::new(),

            buffered_keys: BufferedKeys::default(),
            recording_macro: None,
            registers: RegisterCollection::new(),
            read_line: ReadLine::default(),
            picker: Picker::default(),
            string_pool: StringPool::default(),

            status_bar: StatusBar::new(),
            aux_pattern: Pattern::new(),

            commands: CommandManager::new(),
            events: EditorEventQueue::default(),
        }
    }

    pub fn buffer_view_handle_from_path(
        &mut self,
        client_handle: ClientHandle,
        path: &Path,
        properties: BufferProperties,
        create_if_not_found: bool,
    ) -> Result<BufferViewHandle, BufferReadError> {
        if let Some(buffer_handle) = self.buffers.find_with_path(&self.current_directory, path) {
            let handle = self
                .buffer_views
                .buffer_view_handle_from_buffer_handle(client_handle, buffer_handle);
            Ok(handle)
        } else {
            let path = path.strip_prefix(&self.current_directory).unwrap_or(path);
            let buffer = self.buffers.add_new();
            buffer.path.clear();
            buffer.path.push(path);
            buffer.properties = properties;

            match buffer.read_from_file(&mut self.word_database, &mut self.events) {
                Ok(()) => {
                    let handle = self.buffer_views.add_new(client_handle, buffer.handle());
                    Ok(handle)
                }
                Err(BufferReadError::FileNotFound) if create_if_not_found => {
                    let handle = self.buffer_views.add_new(client_handle, buffer.handle());
                    Ok(handle)
                }
                Err(error) => {
                    let handle = buffer.handle();
                    self.buffers.defer_remove(handle, &mut self.events);
                    Err(error)
                }
            }
        }
    }

    pub fn enter_mode(&mut self, next: ModeKind) {
        Mode::change_to(self, next);
    }

    pub fn enter_plugin_mode(&mut self, plugin_handle: PluginHandle) {
        Mode::change_to(self, ModeKind::Plugin);
        self.mode.plugin_handle = Some(plugin_handle);
    }

    pub fn execute_keys(
        ctx: &mut EditorContext,
        client_handle: ClientHandle,
        mut keys: KeysIterator,
    ) -> EditorFlow {
        let start_index = keys.index;

        match ctx.editor.keymaps.matches(
            ctx.editor.mode.kind(),
            &ctx.editor.buffered_keys.0[start_index..],
        ) {
            MatchResult::None => (),
            MatchResult::Prefix => return EditorFlow::Continue,
            MatchResult::ReplaceWith(replaced_keys) => {
                ctx.editor.buffered_keys.0.truncate(start_index);
                ctx.editor.buffered_keys.0.extend_from_slice(replaced_keys);
            }
        }

        loop {
            if keys.index == ctx.editor.buffered_keys.0.len() {
                break;
            }
            let from_index = ctx.editor.recording_macro.map(|_| keys.index);

            match Mode::on_keys(ctx, client_handle, &mut keys) {
                None => return EditorFlow::Continue,
                Some(EditorFlow::Continue) => (),
                Some(flow) => {
                    ctx.editor.enter_mode(ModeKind::default());
                    ctx.editor.buffered_keys.0.truncate(start_index);
                    return flow;
                }
            }

            if let (Some(from_index), Some(register_key)) = (from_index, ctx.editor.recording_macro)
            {
                for key in &ctx.editor.buffered_keys.0[from_index..keys.index] {
                    use fmt::Write;
                    let register = ctx.editor.registers.get_mut(register_key);
                    let _ = write!(register, "{}", key);
                }
            }

            ctx.trigger_event_handlers();
        }

        ctx.editor.buffered_keys.0.truncate(start_index);
        EditorFlow::Continue
    }

    pub(crate) fn on_client_event(
        ctx: &mut EditorContext,
        client_handle: ClientHandle,
        event: ClientEvent,
    ) -> EditorFlow {
        match event {
            ClientEvent::Key(target, key) => {
                let client_handle = match target {
                    TargetClient::Sender => client_handle,
                    TargetClient::Focused => match ctx.clients.focused_client() {
                        Some(handle) => handle,
                        None => return EditorFlow::Continue,
                    },
                };

                if ctx.clients.focus_client(client_handle) {
                    ctx.editor.recording_macro = None;
                    ctx.editor.buffered_keys.0.clear();
                    ctx.editor.enter_mode(ModeKind::default());
                }

                if key != Key::None {
                    ctx.editor.status_bar.clear();
                }
                ctx.editor.buffered_keys.0.push(key);
                Self::execute_keys(ctx, client_handle, KeysIterator { index: 0 })
            }
            ClientEvent::Resize(width, height) => {
                let client = ctx.clients.get_mut(client_handle);
                client.viewport_size = (width, height);
                EditorFlow::Continue
            }
            ClientEvent::Command(target, command) => {
                let client_handle = match target {
                    TargetClient::Sender => client_handle,
                    TargetClient::Focused => match ctx.clients.focused_client() {
                        Some(handle) => handle,
                        None => return EditorFlow::Continue,
                    },
                };

                let mut command = ctx.editor.string_pool.acquire_with(command);
                let flow =
                    CommandManager::eval_and_write_error(ctx, Some(client_handle), &mut command);
                ctx.editor.string_pool.release(command);
                flow
            }
            ClientEvent::StdinInput(target, bytes) => {
                let client_handle = match target {
                    TargetClient::Sender => client_handle,
                    TargetClient::Focused => match ctx.clients.focused_client() {
                        Some(handle) => handle,
                        None => return EditorFlow::Continue,
                    },
                };

                ctx.clients
                    .get_mut(client_handle)
                    .on_stdin_input(&mut ctx.editor, bytes);
                ctx.trigger_event_handlers();
                EditorFlow::Continue
            }
        }
    }

    pub(crate) fn on_idle(&mut self) {
        self.events.enqueue(EditorEvent::Idle);
    }
}
