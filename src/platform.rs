use std::{io, process::Command};

#[cfg(windows)]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    None,
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Delete,
    F(u8),
    Char(char),
    Ctrl(char),
    Alt(char),
    Esc,
}

#[derive(Clone, Copy)]
pub enum ServerEvent {
    Idle,
    Redraw,
    ConnectionOpen { index: usize },
    ConnectionClose { index: usize },
    ConnectionMessage { index: usize, len: usize },
    ProcessStdout { index: usize, len: usize },
    ProcessStderr { index: usize, len: usize },
    ProcessExit { index: usize, success: bool },
}

#[derive(Clone, Copy)]
pub enum ClientEvent {
    Resize(usize, usize),
    Key(Key),
    Message(usize),
}

pub trait Args: Sized {
    fn parse() -> Option<Self>;
    fn session(&self) -> Option<&str>;
}

pub trait ServerApplication: Sized {
    type Args: Args;
    fn connection_buffer_len() -> usize;
    fn new<P>(args: Self::Args, platform: &mut P) -> Self
    where
        P: ServerPlatform;
    fn on_event<P>(&mut self, platform: &mut P, event: ServerEvent) -> bool
    where
        P: ServerPlatform;
}

pub trait ClientApplication: Sized {
    type Args: Args;
    fn connection_buffer_len() -> usize;
    fn new<P>(args: Self::Args, platform: &mut P) -> Self
    where
        P: ClientPlatform;
    fn on_events<P>(&mut self, platform: &mut P, event: &[ClientEvent]) -> bool
    where
        P: ClientPlatform;
}

pub trait ServerPlatform {
    fn request_redraw(&mut self);

    fn read_from_clipboard(&self) -> Option<&str>;
    fn write_to_clipboard(&self, text: &str);

    fn read_from_connection(&self, index: usize, len: usize) -> &[u8];
    fn write_to_connection(&mut self, index: usize, buf: &[u8]) -> bool;
    fn close_connection(&mut self, index: usize);

    fn spawn_process(
        &mut self,
        command: Command,
        stdout_buf_len: usize,
        stderr_buf_len: usize,
    ) -> io::Result<usize>;
    fn read_from_process_stdout(&self, index: usize, len: usize) -> &[u8];
    fn read_from_process_stderr(&self, index: usize, len: usize) -> &[u8];
    fn write_to_process(&mut self, index: usize, buf: &[u8]) -> bool;
    fn kill_process(&mut self, index: usize);
}

pub trait ClientPlatform {
    fn read(&self, len: usize) -> &[u8];
    fn write(&mut self, buf: &[u8]) -> bool;
}

pub fn run<A, S, C>()
where
    A: Args,
    S: ServerApplication<Args = A>,
    C: ClientApplication<Args = A>,
{
    #[cfg(windows)]
    {
        windows::run::<A, S, C>();
    }
}
