use std::{
    io,
    ops::{Deref, DerefMut},
    path::PathBuf,
    process::{Command, Stdio},
};

use pepper::{
    editor::EditorContext,
    editor_utils::{hash_bytes, parse_process_command, MessageKind},
    events::{EditorEvent, EditorEventIter},
    glob::{Glob, InvalidGlobError},
    platform::{Platform, PlatformProcessHandle, PlatformRequest, ProcessTag},
    plugin::{CompletionContext, Plugin, PluginDefinition, PluginHandle},
    ResourceFile,
};

mod capabilities;
mod client;
mod client_event_handler;
mod command;
mod json;
mod mode;
mod protocol;

use client::{Client, ClientHandle};
use json::{JsonObject, JsonValue};
use protocol::{ProtocolError, ResponseError, ServerEvent};

const SERVER_PROCESS_BUFFER_LEN: usize = 4 * 1024;

pub static DEFAULT_BINDINGS_CONFIG: ResourceFile = ResourceFile {
    name: "lsp_default_bindings.pepper",
    content: include_str!("../rc/default_bindings.pepper"),
};

pub static DEFINITION: PluginDefinition = PluginDefinition {
    instantiate: |handle, ctx| {
        command::register_commands(&mut ctx.editor.commands, handle);
        Some(Plugin {
            data: Box::new(LspPlugin::default()),

            on_editor_events,

            on_process_spawned,
            on_process_output,
            on_process_exit,

            on_completion,

            ..Default::default()
        })
    },
    help_pages: &[ResourceFile {
        name: "lsp_help.md",
        content: include_str!("../rc/help.md"),
    }],
};

struct ClientRecipe {
    glob_hash: u64,
    glob: Glob,
    command: String,
    root: PathBuf,
    log_file_path: String,
    running_client: Option<ClientHandle>,
}

enum ClientEntry {
    Occupied(Box<Client>),
    Reserved,
    Vacant,
}
impl ClientEntry {
    pub fn reserve_and_take(&mut self) -> Option<Box<Client>> {
        match self {
            Self::Occupied(_) => {
                let mut client = ClientEntry::Reserved;
                std::mem::swap(self, &mut client);
                match client {
                    Self::Occupied(client) => Some(client),
                    _ => unreachable!(),
                }
            }
            _ => None,
        }
    }
}

pub(crate) struct ClientGuard(Box<Client>);
impl Deref for ClientGuard {
    type Target = Client;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}
impl DerefMut for ClientGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.deref_mut()
    }
}
impl Drop for ClientGuard {
    fn drop(&mut self) {
        panic!("forgot to call 'release' on LspPlugin with ClientGuard");
    }
}

#[derive(Default)]
pub(crate) struct LspPlugin {
    entries: Vec<ClientEntry>,
    recipes: Vec<ClientRecipe>,
    current_client_handle: Option<ClientHandle>,
}

impl LspPlugin {
    pub fn add_recipe(
        &mut self,
        glob: &str,
        command: &str,
        root: Option<&str>,
        log_file_path: Option<&str>,
    ) -> Result<(), InvalidGlobError> {
        let glob_hash = hash_bytes(glob.as_bytes());
        for recipe in &mut self.recipes {
            if recipe.glob_hash == glob_hash {
                recipe.command.clear();
                recipe.command.push_str(command);
                recipe.root.clear();
                if let Some(path) = root {
                    recipe.root.push(path);
                }
                recipe.log_file_path.clear();
                if let Some(name) = log_file_path {
                    recipe.log_file_path.push_str(name);
                }
                recipe.running_client = None;
                return Ok(());
            }
        }

        let mut recipe_glob = Glob::default();
        recipe_glob.compile(glob)?;
        self.recipes.push(ClientRecipe {
            glob_hash,
            glob: recipe_glob,
            command: command.into(),
            root: root.unwrap_or("").into(),
            log_file_path: log_file_path.unwrap_or("").into(),
            running_client: None,
        });
        Ok(())
    }

    pub fn start(
        &mut self,
        platform: &mut Platform,
        plugin_handle: PluginHandle,
        mut command: Command,
        root: PathBuf,
        log_file_path: Option<String>,
    ) -> ClientHandle {
        fn find_vacant_entry(lsp: &mut LspPlugin) -> ClientHandle {
            for (i, entry) in lsp.entries.iter_mut().enumerate() {
                if let ClientEntry::Vacant = entry {
                    return ClientHandle(i as _);
                }
            }
            let handle = ClientHandle(lsp.entries.len() as _);
            lsp.entries.push(ClientEntry::Vacant);
            handle
        }

        let handle = find_vacant_entry(self);

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        platform.requests.enqueue(PlatformRequest::SpawnProcess {
            tag: ProcessTag::Plugin {
                plugin_handle,
                id: handle.0 as _,
            },
            command,
            buf_len: SERVER_PROCESS_BUFFER_LEN,
        });

        let client = Client::new(handle, root, log_file_path);
        self.entries[handle.0 as usize] = ClientEntry::Occupied(Box::new(client));
        handle
    }

    pub fn stop(&mut self, platform: &mut Platform, handle: ClientHandle) {
        if let ClientEntry::Occupied(client) = &mut self.entries[handle.0 as usize] {
            let _ = client.notify(platform, "exit", JsonObject::default());
            if let Some(process_handle) = client.protocol.process_handle() {
                platform.requests.enqueue(PlatformRequest::KillProcess {
                    handle: process_handle,
                });
            }

            self.entries[handle.0 as usize] = ClientEntry::Vacant;
            for recipe in &mut self.recipes {
                if recipe.running_client == Some(handle) {
                    recipe.running_client = None;
                }
            }
        }
    }

    pub fn stop_all(&mut self, platform: &mut Platform) {
        for i in 0..self.entries.len() {
            self.stop(platform, ClientHandle(i as _));
        }
    }

    pub(crate) fn get_mut(&mut self, handle: ClientHandle) -> Option<&mut Client> {
        match &mut self.entries[handle.0 as usize] {
            ClientEntry::Occupied(client) => Some(client.deref_mut()),
            _ => None,
        }
    }

    pub(crate) fn acquire(&mut self, handle: ClientHandle) -> Option<ClientGuard> {
        self.entries[handle.0 as usize]
            .reserve_and_take()
            .map(ClientGuard)
    }

    pub(crate) fn release(&mut self, mut guard: ClientGuard) {
        let index = guard.handle().0 as usize;
        let raw = guard.deref_mut() as *mut _;
        std::mem::forget(guard);
        let client = unsafe { Box::from_raw(raw) };
        self.entries[index] = ClientEntry::Occupied(client);
    }

    pub(crate) fn find_client<P>(&mut self, mut predicate: P) -> Option<ClientGuard>
    where
        P: FnMut(&Client) -> bool,
    {
        for entry in &mut self.entries {
            if let ClientEntry::Occupied(c) = entry {
                if predicate(c) {
                    let client = entry.reserve_and_take().unwrap();
                    return Some(ClientGuard(client));
                }
            }
        }

        None
    }
}

fn on_editor_events(plugin_handle: PluginHandle, ctx: &mut EditorContext) {
    let lsp = ctx.plugins.get_as::<LspPlugin>(plugin_handle);

    let mut events = EditorEventIter::new();
    while let Some(event) = events.next(&ctx.editor.events) {
        if let EditorEvent::BufferRead { handle } = *event {
            let buffer_path = match ctx.editor.buffers.get(handle).path.to_str() {
                Some(path) => path,
                None => continue,
            };
            let (index, recipe) = match lsp
                .recipes
                .iter_mut()
                .enumerate()
                .find(|(_, r)| r.glob.matches(buffer_path))
            {
                Some(recipe) => recipe,
                None => continue,
            };
            if recipe.running_client.is_some() {
                continue;
            }
            let command = match parse_process_command(&recipe.command) {
                Some(command) => command,
                None => {
                    ctx.editor
                        .status_bar
                        .write(MessageKind::Error)
                        .fmt(format_args!("invalid lsp command '{}'", &recipe.command));
                    continue;
                }
            };

            let root = if recipe.root.as_os_str().is_empty() {
                ctx.editor.current_directory.clone()
            } else {
                recipe.root.clone()
            };

            let log_file_path = if recipe.log_file_path.is_empty() {
                None
            } else {
                Some(recipe.log_file_path.clone())
            };

            let client_handle = lsp.start(
                &mut ctx.platform,
                plugin_handle,
                command,
                root,
                log_file_path,
            );
            lsp.recipes[index].running_client = Some(client_handle);
        }
    }

    for entry in &mut lsp.entries {
        if let ClientEntry::Occupied(client) = entry {
            client.on_editor_events(&mut ctx.editor, &mut ctx.platform);
        }
    }
}

fn on_process_spawned(
    handle: PluginHandle,
    ctx: &mut EditorContext,
    client_index: u32,
    process_handle: PlatformProcessHandle,
) {
    if let ClientEntry::Occupied(client) =
        &mut ctx.plugins.get_as::<LspPlugin>(handle).entries[client_index as usize]
    {
        client.protocol.set_process_handle(process_handle);
        client.initialize(&mut ctx.platform);
    }
}

fn on_process_output(
    plugin_handle: PluginHandle,
    ctx: &mut EditorContext,
    client_index: u32,
    bytes: &[u8],
) {
    let lsp = ctx.plugins.get_as::<LspPlugin>(plugin_handle);
    let mut client_guard = match lsp.acquire(ClientHandle(client_index as _)) {
        Some(client) => client,
        None => return,
    };
    let client = client_guard.deref_mut();

    let mut events = client.protocol.parse_events(bytes);
    while let Some(event) = events.next(&mut client.protocol, &mut client.json) {
        match event {
            ServerEvent::ParseError => {
                client.write_to_log_file(|buf, json| {
                    use io::Write;
                    let _ = write!(buf, "send parse error\nrequest_id: ");
                    let _ = json.write(buf, &JsonValue::Null);
                });
                client.respond(
                    &mut ctx.platform,
                    JsonValue::Null,
                    Err(ResponseError::parse_error()),
                );
            }
            ServerEvent::Request(request) => {
                let request_id = request.id.clone();
                match client_event_handler::on_request(client, ctx, request) {
                    Ok(value) => client.respond(&mut ctx.platform, request_id, Ok(value)),
                    Err(ProtocolError::ParseError) => {
                        client.respond(
                            &mut ctx.platform,
                            request_id,
                            Err(ResponseError::parse_error()),
                        );
                    }
                    Err(ProtocolError::MethodNotFound) => {
                        client.respond(
                            &mut ctx.platform,
                            request_id,
                            Err(ResponseError::method_not_found()),
                        );
                    }
                }
            }
            ServerEvent::Notification(notification) => {
                let _ =
                    client_event_handler::on_notification(client, ctx, plugin_handle, notification);
            }
            ServerEvent::Response(response) => {
                let _ = client_event_handler::on_response(client, ctx, plugin_handle, response);
            }
        }
    }
    events.finish(&mut client.protocol);

    let lsp = ctx.plugins.get_as::<LspPlugin>(plugin_handle);
    lsp.release(client_guard);
}

fn on_process_exit(handle: PluginHandle, ctx: &mut EditorContext, client_index: u32) {
    for buffer in ctx.editor.buffers.iter_mut() {
        let mut lints = buffer.lints.mut_guard(handle);
        lints.clear();
    }

    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
    if let ClientEntry::Occupied(client) = &mut lsp.entries[client_index as usize] {
        client.write_to_log_file(|buf, _| {
            use io::Write;
            let _ = write!(buf, "lsp server stopped");
        });

        let client_handle = client.handle();
        for recipe in &mut lsp.recipes {
            if recipe.running_client == Some(client_handle) {
                recipe.running_client = None;
            }
        }
    }
}

fn on_completion(
    handle: PluginHandle,
    ctx: &mut EditorContext,
    completion_ctx: &CompletionContext,
) -> bool {
    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
    for entry in &mut lsp.entries {
        let client = match entry {
            ClientEntry::Occupied(client) => client,
            _ => continue,
        };

        let mut should_complete = completion_ctx.completion_requested;

        if !should_complete {
            if let Some(c) = ctx
                .editor
                .buffers
                .get(completion_ctx.buffer_handle)
                .content()
                .text_range(completion_ctx.word_range)
                .next()
                .and_then(|s| s.chars().next_back())
            {
                if client.signature_help_triggers().contains(c) {
                    client.signature_help(
                        &ctx.editor,
                        &mut ctx.platform,
                        completion_ctx.buffer_handle,
                        completion_ctx.cursor_position,
                    );
                    return false;
                }

                should_complete = client.completion_triggers().contains(c);
            }
        }

        if should_complete {
            client.completion(
                &ctx.editor,
                &mut ctx.platform,
                completion_ctx.client_handle,
                completion_ctx.buffer_handle,
                completion_ctx.cursor_position,
            );
            return true;
        }
    }

    false
}

