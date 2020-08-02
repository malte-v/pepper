use std::{cmp::Ordering, io::Write, iter, sync::mpsc, thread};

use crossterm::{
    cursor, event, handle_command,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal, ErrorKind, Result,
};

use crate::{
    application::{UiError, UI},
    buffer_position::BufferPosition,
    client::Client,
    event::{Event, Key},
    mode::Mode,
    syntax::TokenKind,
    theme,
};

fn convert_event(event: event::Event) -> Event {
    match event {
        event::Event::Key(e) => match e.code {
            event::KeyCode::Backspace => Event::Key(Key::Backspace),
            event::KeyCode::Enter => Event::Key(Key::Enter),
            event::KeyCode::Left => Event::Key(Key::Left),
            event::KeyCode::Right => Event::Key(Key::Right),
            event::KeyCode::Up => Event::Key(Key::Up),
            event::KeyCode::Down => Event::Key(Key::Down),
            event::KeyCode::Home => Event::Key(Key::Home),
            event::KeyCode::End => Event::Key(Key::End),
            event::KeyCode::PageUp => Event::Key(Key::PageUp),
            event::KeyCode::PageDown => Event::Key(Key::PageDown),
            event::KeyCode::Tab => Event::Key(Key::Tab),
            event::KeyCode::Delete => Event::Key(Key::Delete),
            event::KeyCode::F(f) => Event::Key(Key::F(f)),
            event::KeyCode::Char(c) => match e.modifiers {
                event::KeyModifiers::CONTROL => Event::Key(Key::Ctrl(c)),
                event::KeyModifiers::ALT => Event::Key(Key::Alt(c)),
                _ => Event::Key(Key::Char(c)),
            },
            event::KeyCode::Esc => Event::Key(Key::Esc),
            _ => Event::None,
        },
        event::Event::Resize(w, h) => Event::Resize(w, h),
        _ => Event::None,
    }
}

const fn convert_color(color: theme::Color) -> Color {
    Color::Rgb {
        r: color.0,
        g: color.1,
        b: color.2,
    }
}

impl UiError for ErrorKind {}

pub struct Tui<W>
where
    W: Write,
{
    write: W,
    scroll: usize,
    width: u16,
    height: u16,
}

impl<W> Tui<W>
where
    W: Write,
{
    pub fn new(write: W) -> Self {
        Self {
            write,
            scroll: 0,
            width: 0,
            height: 0,
        }
    }
}

impl<W> UI for Tui<W>
where
    W: Write,
{
    type Error = ErrorKind;

    fn run_event_loop_in_background(
        event_sender: mpsc::Sender<Event>,
    ) -> thread::JoinHandle<Result<()>> {
        thread::spawn(move || {
            while event_sender.send(convert_event(event::read()?)).is_ok() {}
            Ok(())
        })
    }

    fn init(&mut self) -> Result<()> {
        handle_command!(self.write, terminal::EnterAlternateScreen)?;
        self.write.flush()?;
        handle_command!(self.write, cursor::Hide)?;
        self.write.flush()?;
        terminal::enable_raw_mode()?;

        let size = terminal::size()?;
        self.resize(size.0, size.1)
    }

    fn resize(&mut self, width: u16, height: u16) -> Result<()> {
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn draw(&mut self, client: &Client, error: Option<String>) -> Result<()> {
        let cursor_position = client.main_cursor.position;
        let height = self.height - 1;
        if cursor_position.line_index < self.scroll {
            self.scroll = cursor_position.line_index;
        } else if cursor_position.line_index >= self.scroll + height as usize {
            self.scroll = cursor_position.line_index - height as usize + 1;
        }

        draw(
            &mut self.write,
            client,
            self.scroll,
            self.width,
            self.height,
            error,
        )
    }

    fn shutdown(&mut self) -> Result<()> {
        handle_command!(self.write, ResetColor)?;
        handle_command!(
            self.write,
            terminal::Clear(terminal::ClearType::UntilNewLine)
        )?;
        handle_command!(self.write, terminal::LeaveAlternateScreen)?;
        handle_command!(self.write, cursor::Show)?;
        terminal::disable_raw_mode()?;
        Ok(())
    }
}

fn draw<W>(
    write: &mut W,
    client: &Client,
    scroll: usize,
    width: u16,
    height: u16,
    error: Option<String>,
) -> Result<()>
where
    W: Write,
{
    enum DrawState {
        Normal,
        Selection,
        Highlight,
        Cursor,
    }

    let theme = &client.config.theme;

    handle_command!(write, cursor::Hide)?;

    let cursor_color = match client.mode {
        Mode::Select => convert_color(theme.cursor_select),
        Mode::Insert => convert_color(theme.cursor_insert),
        _ => convert_color(theme.cursor_normal),
    };

    let background_color = convert_color(theme.background);
    let text_normal_color = convert_color(theme.text_normal);
    let highlight_color = convert_color(theme.highlight);

    let mut current_token_kind = TokenKind::Text;
    let mut text_color = text_normal_color;

    handle_command!(write, cursor::MoveTo(0, 0))?;
    handle_command!(write, SetBackgroundColor(background_color))?;
    handle_command!(write, SetForegroundColor(text_color))?;

    let mut line_index = scroll;
    let mut drawn_line_count = 0;

    'lines_loop: for line in client.buffer.lines_from(line_index) {
        let mut draw_state = DrawState::Normal;
        let mut column_index = 0;
        let mut x = 0;

        for c in line.text.chars().chain(iter::once(' ')) {
            if x >= width {
                handle_command!(write, cursor::MoveToNextLine(1))?;

                drawn_line_count += 1;
                x -= width;

                if drawn_line_count >= height - 1 {
                    break 'lines_loop;
                }
            }

            let char_position = BufferPosition::line_col(line_index, column_index);

            let token_kind = client.highlighted_buffer.find_token_kind_at(char_position);
            if token_kind != current_token_kind {
                current_token_kind = token_kind;
                text_color = match token_kind {
                    TokenKind::Text => text_normal_color,
                    TokenKind::Comment => text_normal_color,
                    TokenKind::Keyword => text_normal_color,
                    TokenKind::Modifier => text_normal_color,
                    TokenKind::Symbol => text_normal_color,
                    TokenKind::String => text_normal_color,
                    TokenKind::Char => text_normal_color,
                    TokenKind::Literal => text_normal_color,
                    TokenKind::Number => text_normal_color,
                };
            }

            if client.cursors[..]
                .binary_search_by_key(&char_position, |c| c.position)
                .is_ok()
            {
                if !matches!(draw_state, DrawState::Cursor) {
                    draw_state = DrawState::Cursor;
                    handle_command!(write, SetBackgroundColor(cursor_color))?;
                    handle_command!(write, SetForegroundColor(text_color))?;
                }
            } else if client.cursors[..]
                .binary_search_by(|c| {
                    let range = c.range();
                    if range.to < char_position {
                        Ordering::Less
                    } else if range.from > char_position {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                })
                .is_ok()
            {
                if !matches!(draw_state, DrawState::Selection) {
                    draw_state = DrawState::Selection;
                    handle_command!(write, SetBackgroundColor(text_color))?;
                    handle_command!(write, SetForegroundColor(background_color))?;
                }
            } else if client
                .search_ranges
                .binary_search_by(|r| {
                    if r.to < char_position {
                        Ordering::Less
                    } else if r.from > char_position {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                })
                .is_ok()
            {
                if !matches!(draw_state, DrawState::Highlight) {
                    draw_state = DrawState::Highlight;
                    handle_command!(write, SetBackgroundColor(highlight_color))?;
                    handle_command!(write, SetForegroundColor(text_color))?;
                }
            } else if !matches!(draw_state, DrawState::Normal) {
                draw_state = DrawState::Normal;
                handle_command!(write, SetBackgroundColor(background_color))?;
                handle_command!(write, SetForegroundColor(text_color))?;
            }

            match c {
                '\t' => {
                    for _ in 0..client.config.tab_size {
                        handle_command!(write, Print(' '))?
                    }
                    x += client.config.tab_size as u16;
                }
                _ => {
                    handle_command!(write, Print(c))?;
                    x += 1;
                }
            }

            column_index += 1;
        }

        if x < width {
            handle_command!(write, SetBackgroundColor(background_color))?;
            handle_command!(write, terminal::Clear(terminal::ClearType::UntilNewLine))?;
        }

        handle_command!(write, cursor::MoveToNextLine(1))?;

        line_index += 1;
        drawn_line_count += 1;

        if drawn_line_count >= height - 1 {
            break;
        }
    }

    handle_command!(write, SetBackgroundColor(background_color))?;
    handle_command!(write, SetForegroundColor(text_color))?;
    for _ in drawn_line_count..(height - 1) {
        handle_command!(write, Print('~'))?;
        handle_command!(write, terminal::Clear(terminal::ClearType::UntilNewLine))?;
        handle_command!(write, cursor::MoveToNextLine(1))?;
    }

    handle_command!(write, cursor::MoveToNextLine(1))?;
    draw_statusbar(write, client, width, error)?;

    write.flush()?;
    Ok(())
}

fn draw_statusbar<W>(
    write: &mut W,
    client: &Client,
    width: u16,
    error: Option<String>,
) -> Result<()>
where
    W: Write,
{
    fn draw_input<W>(
        write: &mut W,
        prefix: &str,
        input: &str,
        background_color: Color,
        cursor_color: Color,
    ) -> Result<usize>
    where
        W: Write,
    {
        handle_command!(write, Print(prefix))?;
        handle_command!(write, Print(input))?;
        handle_command!(write, SetBackgroundColor(cursor_color))?;
        handle_command!(write, Print(' '))?;
        handle_command!(write, SetBackgroundColor(background_color))?;
        Ok(prefix.len() + input.len() + 1)
    }

    fn find_digit_count(mut number: usize) -> usize {
        let mut count = 0;
        while number > 0 {
            number /= 10;
            count += 1;
        }
        count
    }

    let background_color = convert_color(client.config.theme.text_normal);
    let foreground_color = convert_color(client.config.theme.background);
    let cursor_color = convert_color(client.config.theme.cursor_normal);

    if client.has_focus {
        handle_command!(write, SetBackgroundColor(background_color))?;
        handle_command!(write, SetForegroundColor(foreground_color))?;
    } else {
        handle_command!(write, SetBackgroundColor(foreground_color))?;
        handle_command!(write, SetForegroundColor(background_color))?;
    }

    let x = if let Some(error) = &error {
        let prefix = "error:";
        handle_command!(write, Print(prefix))?;
        handle_command!(write, Print(error))?;
        prefix.len() + error.len()
    } else if client.has_focus {
        match client.mode {
            Mode::Select => {
                let text = "-- SELECT --";
                handle_command!(write, Print(text))?;
                text.len()
            }
            Mode::Insert => {
                let text = "-- INSERT --";
                handle_command!(write, Print(text))?;
                text.len()
            }
            Mode::Search(_) => draw_input(
                write,
                "search:",
                &client.input[..],
                background_color,
                cursor_color,
            )?,
            Mode::Command(_) => draw_input(
                write,
                "command:",
                &client.input[..],
                background_color,
                cursor_color,
            )?,
            _ => 0,
        }
    } else {
        0
    };

    if let Some(buffer_path) = client
        .path
        .as_ref()
        .map(|p| p.as_os_str().to_str())
        .flatten()
    {
        let line_number = client.main_cursor.position.line_index + 1;
        let column_number = client.main_cursor.position.column_index + 1;
        let line_digit_count = find_digit_count(line_number);
        let column_digit_count = find_digit_count(column_number);
        let skip = (width as usize).saturating_sub(
            x + buffer_path.len() + 1 + line_digit_count + 1 + column_digit_count + 1,
        );
        for _ in 0..skip {
            handle_command!(write, Print(' '))?;
        }

        handle_command!(write, Print(buffer_path))?;
        handle_command!(write, Print(':'))?;
        handle_command!(write, Print(line_number))?;
        handle_command!(write, Print(','))?;
        handle_command!(write, Print(column_number))?;
    }

    handle_command!(write, terminal::Clear(terminal::ClearType::UntilNewLine))?;
    Ok(())
}
