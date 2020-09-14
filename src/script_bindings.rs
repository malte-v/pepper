use std::{
    fmt,
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
};

use crate::{
    editor::{EditorLoop, StatusMessageKind},
    keymap::ParseKeyMapError,
    mode::Mode,
    pattern::Pattern,
    script::{
        ScriptContext, ScriptEngineRef, ScriptError, ScriptObject, ScriptResult, ScriptStr,
        ScriptValue,
    },
};

pub struct QuitError;
impl fmt::Display for QuitError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("could not quit now")
    }
}

pub fn bind_all(scripts: ScriptEngineRef) -> ScriptResult<()> {
    macro_rules! register {
        (global => $($func:ident,)*) => {
            let globals = scripts.globals_object();
            $(
                let func = scripts.create_ctx_function(global::$func)?;
                globals.set(stringify!($func), ScriptValue::Function(func))?;
            )*
        };
        ($obj:ident => $($func:ident,)*) => {
            let $obj = scripts.create_object()?;
            $(
                let func = scripts.create_ctx_function($obj::$func)?;
                $obj.set(stringify!($func), ScriptValue::Function(func))?;
            )*
        };
    }

    macro_rules! register_object {
        ($name:ident) => {
            let $name = scripts.create_object()?;
            let meta = scripts.create_object()?;
            meta.set(
                "__index",
                ScriptValue::Function(scripts.create_ctx_function($name::index)?),
            )?;
            meta.set(
                "__newindex",
                ScriptValue::Function(scripts.create_ctx_function($name::newindex)?),
            )?;
            $name.set_meta_object(Some(meta));
            scripts
                .globals_object()
                .set(stringify!($name), ScriptValue::Object($name))?;
        };
    }

    register!(global => print, quit, quit_all, open, close, close_all, save, save_all,);
    register!(client => index,);
    register!(editor => selection, delete_selection, insert_text,);
    register!(process => pipe, spawn,);
    register!(keymap => normal, select, insert,);
    register!(syntax => extension, rule,);

    register_object!(config);
    register_object!(theme);

    Ok(())
}

mod global {
    use super::*;

    pub fn print(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        value: ScriptValue,
    ) -> ScriptResult<()> {
        let message = value.to_string();
        *ctx.status_message_kind = StatusMessageKind::Info;
        ctx.status_message.clear();
        ctx.status_message.push_str(&message);
        Ok(())
    }

    pub fn quit(_engine: ScriptEngineRef, ctx: &mut ScriptContext, _: ()) -> ScriptResult<()> {
        *ctx.editor_loop = EditorLoop::Quit;
        Err(ScriptError::from(QuitError))
    }

    pub fn quit_all(_engine: ScriptEngineRef, ctx: &mut ScriptContext, _: ()) -> ScriptResult<()> {
        *ctx.editor_loop = EditorLoop::QuitAll;
        Err(ScriptError::from(QuitError))
    }

    pub fn open(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        path: ScriptStr,
    ) -> ScriptResult<()> {
        let path = Path::new(path.to_str()?);
        let buffer_view_handle = ctx
            .buffer_views
            .new_buffer_from_file(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                ctx.target_client,
                path,
            )
            .map_err(ScriptError::from)?;
        ctx.set_current_buffer_view_handle(Some(buffer_view_handle));
        Ok(())
    }

    pub fn close(_engine: ScriptEngineRef, ctx: &mut ScriptContext, _: ()) -> ScriptResult<()> {
        if let Some(handle) = ctx
            .current_buffer_view_handle()
            .and_then(|h| ctx.buffer_views.get(h))
            .map(|v| v.buffer_handle)
        {
            ctx.buffer_views
                .remove_where(ctx.buffers, ctx.word_database, |view| {
                    view.buffer_handle == handle
                });
        }

        ctx.set_current_buffer_view_handle(None);
        Ok(())
    }

    pub fn close_all(_engine: ScriptEngineRef, ctx: &mut ScriptContext, _: ()) -> ScriptResult<()> {
        ctx.buffer_views
            .remove_where(ctx.buffers, ctx.word_database, |_| true);
        for c in ctx.clients.client_refs() {
            c.client.current_buffer_view_handle = None;
        }
        Ok(())
    }

    pub fn save(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        path: Option<ScriptStr>,
    ) -> ScriptResult<()> {
        let buffer_handle = match ctx
            .current_buffer_view_handle()
            .and_then(|h| ctx.buffer_views.get(h))
            .map(|v| v.buffer_handle)
        {
            Some(handle) => handle,
            None => return Err(ScriptError::from("no buffer opened")),
        };

        let buffer = match ctx.buffers.get_mut(buffer_handle) {
            Some(buffer) => buffer,
            None => return Err(ScriptError::from("no buffer opened")),
        };

        match path {
            Some(path) => {
                let path = Path::new(path.to_str()?);
                buffer.set_path(&ctx.config.syntaxes, path);
                buffer.save_to_file().map_err(ScriptError::from)?;
                Ok(())
            }
            None => buffer.save_to_file().map_err(ScriptError::from),
        }
    }

    pub fn save_all(_engine: ScriptEngineRef, ctx: &mut ScriptContext, _: ()) -> ScriptResult<()> {
        for buffer in ctx.buffers.iter() {
            buffer.save_to_file().map_err(ScriptError::from)?;
        }
        Ok(())
    }
}

mod client {
    use super::*;

    pub fn index(_engine: ScriptEngineRef, ctx: &mut ScriptContext, _: ()) -> ScriptResult<usize> {
        Ok(ctx.target_client.into_index())
    }
}

mod editor {
    use super::*;

    pub fn selection(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        _: (),
    ) -> ScriptResult<String> {
        let mut selection = String::new();
        ctx.current_buffer_view_handle()
            .and_then(|h| ctx.buffer_views.get(h))
            .map(|v| v.get_selection_text(ctx.buffers, &mut selection));
        Ok(selection)
    }

    pub fn delete_selection(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        _: (),
    ) -> ScriptResult<()> {
        if let Some(handle) = ctx.current_buffer_view_handle() {
            ctx.buffer_views.delete_in_selection(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                handle,
            );
        }
        Ok(())
    }

    pub fn insert_text(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        text: ScriptStr,
    ) -> ScriptResult<()> {
        if let Some(handle) = ctx.current_buffer_view_handle() {
            let text = text.to_str()?;
            ctx.buffer_views.insert_text(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                handle,
                text,
            );
        }
        Ok(())
    }
}

mod process {
    use super::*;

    pub fn pipe(
        _engine: ScriptEngineRef,
        _ctx: &mut ScriptContext,
        (name, args, input): (ScriptStr, Vec<ScriptStr>, Option<ScriptStr>),
    ) -> ScriptResult<String> {
        let child = run_process(name, args, input, Stdio::piped())?;
        let child_output = child.wait_with_output().map_err(ScriptError::from)?;
        if child_output.status.success() {
            let child_output = String::from_utf8_lossy(&child_output.stdout);
            Ok(child_output.into_owned())
        } else {
            let child_output = String::from_utf8_lossy(&child_output.stdout);
            Err(ScriptError::from(child_output.into_owned()))
        }
    }

    pub fn spawn(
        _engine: ScriptEngineRef,
        _ctx: &mut ScriptContext,
        (name, args, input): (ScriptStr, Vec<ScriptStr>, Option<ScriptStr>),
    ) -> ScriptResult<()> {
        run_process(name, args, input, Stdio::null())?;
        Ok(())
    }

    fn run_process(
        name: ScriptStr,
        args: Vec<ScriptStr>,
        input: Option<ScriptStr>,
        output: Stdio,
    ) -> ScriptResult<Child> {
        let mut command = Command::new(name.to_str()?);
        command.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(output);
        command.stderr(Stdio::piped());
        for arg in args {
            command.arg(arg.to_str()?);
        }

        let mut child = command.spawn().map_err(ScriptError::from)?;
        if let Some(stdin) = child.stdin.as_mut() {
            let bytes = match input.as_ref() {
                Some(input) => input.as_bytes(),
                None => &[],
            };
            let _ = stdin.write_all(bytes);
        }
        child.stdin = None;
        Ok(child)
    }
}

mod config {
    use super::*;

    pub fn index<'script>(
        engine: ScriptEngineRef<'script>,
        ctx: &mut ScriptContext,
        (_object, index): (ScriptObject, ScriptStr),
    ) -> ScriptResult<ScriptValue<'script>> {
        ctx.config.values.get_from_name(engine, index.to_str()?)
    }

    pub fn newindex(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (_object, index, value): (ScriptObject, ScriptStr, ScriptValue),
    ) -> ScriptResult<()> {
        ctx.config.values.set_from_name(index.to_str()?, value);
        Ok(())
    }
}

mod keymap {
    use super::*;

    pub fn normal(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (from, to): (ScriptStr, ScriptStr),
    ) -> ScriptResult<()> {
        map_mode(ctx, Mode::Normal, from, to)
    }

    pub fn select(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (from, to): (ScriptStr, ScriptStr),
    ) -> ScriptResult<()> {
        map_mode(ctx, Mode::Select, from, to)
    }

    pub fn insert(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (from, to): (ScriptStr, ScriptStr),
    ) -> ScriptResult<()> {
        map_mode(ctx, Mode::Insert, from, to)
    }

    fn map_mode(
        ctx: &mut ScriptContext,
        mode: Mode,
        from: ScriptStr,
        to: ScriptStr,
    ) -> ScriptResult<()> {
        let from = from.to_str()?;
        let to = to.to_str()?;

        match ctx.keymaps.parse_map(mode.discriminant(), from, to) {
            Ok(()) => Ok(()),
            Err(ParseKeyMapError::From(e)) => {
                let message = helper::parsing_error(e.error, from, e.index);
                Err(ScriptError::from(message))
            }
            Err(ParseKeyMapError::To(e)) => {
                let message = helper::parsing_error(e.error, to, e.index);
                Err(ScriptError::from(message))
            }
        }
    }
}

mod theme {
    use super::*;

    pub fn index<'script>(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (_object, index): (ScriptObject, ScriptStr),
    ) -> ScriptResult<ScriptValue<'script>> {
        Ok(ScriptValue::Nil)
    }

    pub fn newindex(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (_object, index, value): (ScriptObject, ScriptStr, ScriptValue),
    ) -> ScriptResult<()> {
        Ok(())
    }
}

mod syntax {
    use super::*;

    pub fn extension(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (main_extension, other_extension): (ScriptStr, ScriptStr),
    ) -> ScriptResult<()> {
        let main_extension = main_extension.to_str()?;
        let other_extension = other_extension.to_str()?;
        ctx.config
            .syntaxes
            .get_by_extension(main_extension)
            .add_extension(other_extension.into());
        Ok(())
    }

    pub fn rule(
        _engine: ScriptEngineRef,
        ctx: &mut ScriptContext,
        (main_extension, token_kind, pattern): (ScriptStr, ScriptStr, ScriptStr),
    ) -> ScriptResult<()> {
        let main_extension = main_extension.to_str()?;
        let token_kind = token_kind.to_str()?;
        let pattern = pattern.to_str()?;

        let token_kind = token_kind.parse().map_err(ScriptError::from)?;
        let pattern = Pattern::new(pattern).map_err(|e| {
            let message = helper::parsing_error(e, pattern, 0);
            ScriptError::from(message)
        })?;

        ctx.config
            .syntaxes
            .get_by_extension(main_extension)
            .add_rule(token_kind, pattern);
        Ok(())
    }
}

mod helper {
    use super::*;

    pub fn parsing_error<T>(
        message: T,
        text: &str,
        error_index: usize,
    ) -> String
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
}
