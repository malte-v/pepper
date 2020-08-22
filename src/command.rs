use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::{Read, Write},
    ops::Range,
    path::Path,
    process::{Command, Stdio},
};

use crate::{
    buffer::{Buffer, BufferCollection, BufferContent, TextRef},
    buffer_view::{BufferView, BufferViewCollection, BufferViewHandle},
    config::{Config, ParseConfigError},
    connection::TargetClient,
    editor_operation::{EditorOperation, EditorOperationSerializer},
    keymap::{KeyMapCollection, ParseKeyMapError},
    mode::Mode,
    pattern::Pattern,
    syntax::TokenKind,
    theme::ParseThemeError,
};

type FullCommandResult = Result<CommandOperation, String>;
type ConfigCommandResult = Result<(), String>;

pub enum CommandOperation {
    Complete,
    Quit,
}

pub struct FullCommandContext<'a> {
    pub target_client: TargetClient,
    pub operations: &'a mut EditorOperationSerializer,

    pub config: &'a Config,
    pub keymaps: &'a mut KeyMapCollection,
    pub buffers: &'a mut BufferCollection,
    pub buffer_views: &'a mut BufferViewCollection,
    pub current_buffer_view_handle: &'a mut Option<BufferViewHandle>,
}

pub struct ConfigCommandContext<'a> {
    pub operations: &'a mut EditorOperationSerializer,
    pub config: &'a Config,
    pub keymaps: &'a mut KeyMapCollection,
}

type FullCommandBody =
    fn(&mut FullCommandContext, &mut CommandArgs, Option<&str>, &mut String) -> FullCommandResult;
type ConfigCommandBody = fn(&mut ConfigCommandContext, &mut CommandArgs) -> ConfigCommandResult;

pub struct CommandArgs<'a> {
    raw: &'a str,
}

impl<'a> CommandArgs<'a> {
    pub fn new(raw: &'a str) -> Self {
        Self { raw }
    }
}

macro_rules! assert_empty {
    ($args:expr) => {
        match $args.next() {
            Some(_) => return Err("command expected less arguments".into()),
            None => (),
        }
    };
}

macro_rules! expect_next {
    ($args:expr) => {
        match $args.next() {
            Some(arg) => arg,
            None => return Err(String::from("command expected more arguments")),
        }
    };
}

macro_rules! input_or_next {
    ($args:expr, $input:expr) => {
        $input.or_else(|| $args.next())
    };
}

macro_rules! expect_input_or_next {
    ($args:expr, $input:expr) => {
        if let Some(input) = $input {
            input
        } else {
            expect_next!($args)
        }
    };
}

impl<'a> Iterator for CommandArgs<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        fn find_string_end(s: &str, delim: char) -> Option<Range<usize>> {
            let mut chars = s.char_indices();
            chars.next()?;
            for (i, c) in chars {
                if c == delim {
                    return Some(delim.len_utf8()..i);
                }
            }
            None
        }

        self.raw = self.raw.trim_start();
        if self.raw.is_empty() {
            return None;
        }

        let arg_range = match self.raw.chars().next() {
            Some('|') => 0..0,
            Some('"') => find_string_end(self.raw, '"')?,
            Some('\'') => find_string_end(self.raw, '\'')?,
            _ => match self.raw.find(|c: char| c.is_whitespace()) {
                Some(end) => 0..end,
                None => 0..self.raw.len(),
            },
        };

        let (arg, after) = self.raw.split_at(arg_range.end);
        self.raw = after;

        Some(&arg[arg_range])
    }
}

pub struct CommandCollection {
    full_commands: HashMap<String, FullCommandBody>,
    config_commands: HashMap<String, ConfigCommandBody>,
}

impl Default for CommandCollection {
    fn default() -> Self {
        let mut this = Self {
            full_commands: HashMap::new(),
            config_commands: HashMap::new(),
        };

        macro_rules! register {
            ($register_command:ident => $($name:ident,)*) => {
                $(this.$register_command(stringify!($name).replace('_', "-"), commands::$name);)*
            }
        }

        register! { register_full_command =>
            quit, open, close, save, save_all,
            selection, replace, pipe,
        }

        register! { register_config_command =>
            set, syntax, theme,
            nmap, smap, imap,
        }

        this
    }
}

impl CommandCollection {
    pub fn register_full_command(&mut self, name: String, body: FullCommandBody) {
        self.full_commands.insert(name, body);
    }

    pub fn register_config_command(&mut self, name: String, body: ConfigCommandBody) {
        self.config_commands.insert(name, body);
    }

    fn next_command(command: &str) -> Option<(&str, CommandArgs)> {
        let mut command = command.trim_start();
        match command.chars().next() {
            Some('|') => command = &command[1..].trim_start(),
            None => return None,
            _ => (),
        }

        if let Some(index) = command.find(' ') {
            Some((&command[..index], CommandArgs::new(&command[index..])))
        } else {
            Some((command, CommandArgs::new("")))
        }
    }

    pub fn parse_and_execut_config_command(
        &self,
        ctx: &mut ConfigCommandContext,
        command: &str,
    ) -> ConfigCommandResult {
        let (name, mut args) = match Self::next_command(command) {
            Some((name, args)) => (name, args),
            None => return Err("empty command name".into()),
        };

        if let Some(command) = self.config_commands.get(name) {
            command(ctx, &mut args)
        } else {
            Err(format!("command '{}' not found", name))
        }
    }

    pub fn parse_and_execute_any_command(
        &self,
        ctx: &mut FullCommandContext,
        mut command: &str,
    ) -> FullCommandResult {
        let mut last_result = None;
        let mut input = String::new();
        let mut output = String::new();

        loop {
            let (name, mut args) = match Self::next_command(command) {
                Some((name, args)) => (name, args),
                None => {
                    break match last_result {
                        Some(result) => result,
                        None => Err("empty command name".into()),
                    }
                }
            };

            if let Some(command) = self.full_commands.get(name) {
                let maybe_input = match last_result {
                    Some(_) => Some(&input[..]),
                    None => None,
                };
                output.clear();
                last_result = Some(command(ctx, &mut args, maybe_input, &mut output));
                std::mem::swap(&mut input, &mut output);
            } else if let Some(command) = self.config_commands.get(name) {
                let mut ctx = ConfigCommandContext {
                    operations: ctx.operations,
                    config: ctx.config,
                    keymaps: ctx.keymaps,
                };
                last_result = Some(command(&mut ctx, &mut args).map(|_| CommandOperation::Complete));
                input.clear();
            } else {
                return Err(format!("command '{}' not found", name));
            }

            command = args.raw;
        }
    }
}

mod helper {
    use super::*;

    pub fn parsing_error<T>(message: T, text: &str, error_index: usize) -> String
    where
        T: fmt::Display,
    {
        let (before, after) = text.split_at(error_index);
        match (before.len(), after.len()) {
            (0, 0) => format!("{} at ''", message),
            (_, 0) => format!("{} at '{}' <- here", message, before),
            (0, _) => format!("{} at here -> '{}'", message, after),
            (_, _) => format!("{} at '{}' <- here '{}'", message, before, after),
        }
    }

    pub fn new_buffer_from_content(
        ctx: &mut FullCommandContext,
        path: &Path,
        content: BufferContent,
    ) {
        ctx.operations.serialize_buffer(ctx.target_client, &content);
        ctx.operations
            .serialize(ctx.target_client, &EditorOperation::Path(path));

        let buffer_handle = ctx.buffers.add(Buffer::new(path.into(), content));
        let buffer_view = BufferView::new(ctx.target_client, buffer_handle);
        let buffer_view_handle = ctx.buffer_views.add(buffer_view);
        *ctx.current_buffer_view_handle = Some(buffer_view_handle);
    }

    pub fn new_buffer_from_file(ctx: &mut FullCommandContext, path: &Path) -> Result<(), String> {
        if let Some(buffer_handle) = ctx.buffers.find_with_path(path) {
            let mut iter = ctx
                .buffer_views
                .iter_with_handles()
                .filter_map(|(handle, view)| {
                    if view.buffer_handle == buffer_handle
                        && view.target_client == ctx.target_client
                    {
                        Some((handle, view))
                    } else {
                        None
                    }
                });

            let view = match iter.next() {
                Some((handle, view)) => {
                    *ctx.current_buffer_view_handle = Some(handle);
                    view
                }
                None => {
                    drop(iter);
                    let view = BufferView::new(ctx.target_client, buffer_handle);
                    let view_handle = ctx.buffer_views.add(view);
                    let view = ctx.buffer_views.get(&view_handle);
                    *ctx.current_buffer_view_handle = Some(view_handle);
                    view
                }
            };

            ctx.operations.serialize_buffer(
                ctx.target_client,
                &ctx.buffers.get(buffer_handle).unwrap().content,
            );
            ctx.operations
                .serialize(ctx.target_client, &EditorOperation::Path(path));
            ctx.operations
                .serialize_cursors(ctx.target_client, &view.cursors);
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
                    BufferContent::from_str(&content[..])
                }
                Err(_) => BufferContent::from_str(""),
            };

            new_buffer_from_content(ctx, path, content);
        } else {
            return Err(format!("invalid path {:?}", path));
        }

        Ok(())
    }

    pub fn write_buffer_to_file(buffer: &Buffer, path: &Path) -> Result<(), String> {
        let mut file =
            File::create(path).map_err(|e| format!("could not create file {:?}: {:?}", path, e))?;

        buffer
            .content
            .write(&mut file)
            .map_err(|e| format!("could not write to file {:?}: {:?}", path, e))
    }
}

mod commands {
    use super::*;

    pub fn quit(
        _ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        _input: Option<&str>,
        _output: &mut String,
    ) -> FullCommandResult {
        assert_empty!(args);
        Ok(CommandOperation::Quit)
    }

    pub fn open<'a, 'b>(
        mut ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        input: Option<&str>,
        _output: &mut String,
    ) -> FullCommandResult {
        let path = Path::new(expect_input_or_next!(args, input));
        assert_empty!(args);
        helper::new_buffer_from_file(&mut ctx, path)?;
        Ok(CommandOperation::Complete)
    }

    pub fn close(
        ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        _input: Option<&str>,
        _output: &mut String,
    ) -> FullCommandResult {
        assert_empty!(args);
        if let Some(handle) = ctx
            .current_buffer_view_handle
            .take()
            .map(|h| ctx.buffer_views.get(&h).buffer_handle)
        {
            for view in ctx.buffer_views.iter() {
                if view.buffer_handle == handle {
                    ctx.operations
                        .serialize(view.target_client, &EditorOperation::Buffer(""));
                    ctx.operations
                        .serialize(view.target_client, &EditorOperation::Path(Path::new("")));
                }
            }
            ctx.buffer_views
                .remove_where(|view| view.buffer_handle == handle);
        }

        Ok(CommandOperation::Complete)
    }

    pub fn save(
        ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        input: Option<&str>,
        _output: &mut String,
    ) -> FullCommandResult {
        let view_handle = ctx
            .current_buffer_view_handle
            .as_ref()
            .ok_or_else(|| String::from("no buffer opened"))?;

        let buffer_handle = ctx.buffer_views.get(view_handle).buffer_handle;
        let buffer = ctx
            .buffers
            .get_mut(buffer_handle)
            .ok_or_else(|| String::from("no buffer opened"))?;

        let path = input_or_next!(args, input);
        assert_empty!(args);
        match path {
            Some(path) => {
                let path = Path::new(path);
                helper::write_buffer_to_file(buffer, path)?;
                for view in ctx.buffer_views.iter() {
                    if view.buffer_handle == buffer_handle {
                        ctx.operations
                            .serialize(view.target_client, &EditorOperation::Path(path));
                    }
                }
                buffer.path.clear();
                buffer.path.push(path);
                Ok(CommandOperation::Complete)
            }
            None => {
                if !buffer.path.as_os_str().is_empty() {
                    return Err(String::from("buffer has no path"));
                }
                helper::write_buffer_to_file(buffer, &buffer.path)?;
                Ok(CommandOperation::Complete)
            }
        }
    }

    pub fn save_all(
        ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        _input: Option<&str>,
        _output: &mut String,
    ) -> FullCommandResult {
        assert_empty!(args);
        for buffer in ctx.buffers.iter() {
            if !buffer.path.as_os_str().is_empty() {
                helper::write_buffer_to_file(buffer, &buffer.path)?;
            }
        }

        Ok(CommandOperation::Complete)
    }

    pub fn selection(
        ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        _input: Option<&str>,
        output: &mut String,
    ) -> FullCommandResult {
        assert_empty!(args);
        if let Some(buffer_view) = ctx
            .current_buffer_view_handle
            .as_ref()
            .map(|h| ctx.buffer_views.get(h))
        {
            buffer_view.get_selection_text(ctx.buffers, output);
        }

        Ok(CommandOperation::Complete)
    }

    pub fn replace(
        ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        input: Option<&str>,
        _output: &mut String,
    ) -> FullCommandResult {
        let input = expect_input_or_next!(args, input);
        assert_empty!(args);
        if let Some(handle) = ctx.current_buffer_view_handle {
            ctx.buffer_views
                .delete_in_selection(ctx.buffers, ctx.operations, handle);
            ctx.buffer_views
                .insert_text(ctx.buffers, ctx.operations, handle, TextRef::Str(input));
        }

        Ok(CommandOperation::Complete)
    }

    pub fn pipe(
        _ctx: &mut FullCommandContext,
        args: &mut CommandArgs,
        input: Option<&str>,
        output: &mut String,
    ) -> FullCommandResult {
        let name = expect_next!(args);

        let mut command = Command::new(name);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        for arg in args {
            command.arg(arg);
        }

        let mut child = command.spawn().map_err(|e| e.to_string())?;
        if let (Some(input), Some(stdin)) = (input, child.stdin.as_mut()) {
            let _ = stdin.write_all(input.as_bytes());
        }
        child.stdin = None;

        let child_output = child.wait_with_output().map_err(|e| e.to_string())?;
        if child_output.status.success() {
            let child_output = String::from_utf8_lossy(&child_output.stdout[..]);
            output.push_str(child_output.as_ref());
            Ok(CommandOperation::Complete)
        } else {
            let child_output = String::from_utf8_lossy(&child_output.stdout[..]).into_owned();
            Err(child_output)
        }
    }

    pub fn set(ctx: &mut ConfigCommandContext, args: &mut CommandArgs) -> ConfigCommandResult {
        let name = expect_next!(args);
        let mut previous = "";
        let mut args = args.map(|a| {
            previous = a;
            a
        });

        let mut values = ctx.config.values.clone();
        match values.parse_and_set(name, &mut args) {
            Ok(()) => assert_empty!(args),
            Err(e) => match e {
                ParseConfigError::ConfigNotFound => return Err(helper::parsing_error(e, name, 0)),
                ParseConfigError::ParseError(e) => {
                    return Err(helper::parsing_error(e, previous, 0))
                }
                ParseConfigError::UnexpectedEndOfValues => {
                    return Err(helper::parsing_error(e, previous, previous.len()));
                }
            },
        }

        ctx.operations
            .serialize_config_values(TargetClient::All, &values);
        Ok(())
    }

    pub fn syntax(ctx: &mut ConfigCommandContext, args: &mut CommandArgs) -> ConfigCommandResult {
        let main_extension = expect_next!(args);
        let subcommand = expect_next!(args);
        if subcommand == "extension" {
            for extension in args {
                ctx.operations.serialize(
                    TargetClient::All,
                    &EditorOperation::SyntaxExtension(main_extension, extension),
                );
            }
        } else if let Some(token_kind) = TokenKind::from_str(subcommand) {
            for pattern in args {
                ctx.operations.serialize_syntax_rule(
                    TargetClient::All,
                    main_extension,
                    token_kind,
                    &Pattern::new(pattern).map_err(|e| helper::parsing_error(e, pattern, 0))?,
                );
            }
        } else {
            return Err(format!(
                "no such subcommand '{}'. expected either 'extension' or a token kind",
                subcommand
            ));
        }

        Ok(())
    }

    pub fn theme(ctx: &mut ConfigCommandContext, args: &mut CommandArgs) -> ConfigCommandResult {
        let name = expect_next!(args);
        let color = expect_next!(args);
        assert_empty!(args);

        let mut theme = ctx.config.theme.clone();
        if let Err(e) = theme.parse_and_set(name, color) {
            let context = format!("{} {}", name, color);
            let error_index = match e {
                ParseThemeError::ColorNotFound => 0,
                _ => context.len(),
            };

            return Err(helper::parsing_error(e, &context[..], error_index));
        }

        ctx.operations.serialize_theme(TargetClient::All, &theme);
        Ok(())
    }

    pub fn nmap(ctx: &mut ConfigCommandContext, args: &mut CommandArgs) -> ConfigCommandResult {
        mode_map(ctx, args, Mode::Normal)
    }

    pub fn smap(ctx: &mut ConfigCommandContext, args: &mut CommandArgs) -> ConfigCommandResult {
        mode_map(ctx, args, Mode::Select)
    }

    pub fn imap(ctx: &mut ConfigCommandContext, args: &mut CommandArgs) -> ConfigCommandResult {
        mode_map(ctx, args, Mode::Insert)
    }

    fn mode_map(
        ctx: &mut ConfigCommandContext,
        args: &mut CommandArgs,
        mode: Mode,
    ) -> ConfigCommandResult {
        let from = expect_next!(args);
        let to = expect_next!(args);
        assert_empty!(args);

        match ctx.keymaps.parse_map(mode.discriminant(), from, to) {
            Ok(()) => Ok(()),
            Err(ParseKeyMapError::From(i, e)) => Err(helper::parsing_error(e, from, i)),
            Err(ParseKeyMapError::To(i, e)) => Err(helper::parsing_error(e, to, i)),
        }
    }
}
