use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, FALSE, TRUE},
        ntdef::NULL,
        winerror::WAIT_TIMEOUT,
    },
    um::{
        consoleapi::{GetConsoleMode, ReadConsoleInputW, SetConsoleCtrlHandler, SetConsoleMode},
        fileapi::{CreateFileW, OPEN_EXISTING},
        handleapi::INVALID_HANDLE_VALUE,
        minwinbase::OVERLAPPED,
        namedpipeapi::{CreateNamedPipeW, SetNamedPipeHandleState},
        processenv::GetStdHandle,
        synchapi::{CreateEventW, WaitForMultipleObjects},
        winbase::{
            FILE_FLAG_OVERLAPPED, INFINITE, PIPE_ACCESS_DUPLEX, PIPE_READMODE_MESSAGE,
            PIPE_TYPE_MESSAGE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
        },
        wincon::{
            ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
        },
        wincontypes::{
            INPUT_RECORD, KEY_EVENT, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED,
            RIGHT_CTRL_PRESSED, SHIFT_PRESSED, WINDOW_BUFFER_SIZE_EVENT,
        },
        winnt::{GENERIC_READ, GENERIC_WRITE, HANDLE},
        winuser::{
            VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F24, VK_HOME, VK_LEFT,
            VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
        },
    },
};

use crate::platform::{Key, Platform};

pub fn run() {
    unsafe { run_unsafe() }
}

unsafe fn run_unsafe() {
    unsafe extern "system" fn ctrl_handler(_ctrl_type: DWORD) -> BOOL {
        FALSE
    }

    if SetConsoleCtrlHandler(Some(ctrl_handler), TRUE) == FALSE {
        panic!("could not set ctrl handler");
    }

    let session_name = "session_name";
    let mut pipe_path = Vec::new();
    pipe_path.extend("\\\\.\\pipe\\".encode_utf16());
    pipe_path.extend(session_name.encode_utf16());
    pipe_path.push(0);

    if !try_run_client(&pipe_path) {
        run_server(&pipe_path);
    }
}

unsafe fn run_server(pipe_path: &[u16]) {
    #[derive(Clone, Copy)]
    struct NamedPipe {
        pub handle: HANDLE,
        pub overlapped: OVERLAPPED,
    }

    const MAX_CLIENT_COUNT: usize = 4;
    const PIPE_BUFFER_LEN: usize = 1024 * 2;

    let mut wait_events = [INVALID_HANDLE_VALUE; MAX_CLIENT_COUNT];
    let mut pipes = [std::mem::zeroed::<NamedPipe>(); MAX_CLIENT_COUNT];
    let wait_events = &mut wait_events;

    for i in 0..MAX_CLIENT_COUNT {
        let event_handle = CreateEventW(std::ptr::null_mut(), TRUE, TRUE, std::ptr::null());
        if event_handle == NULL {
            panic!("could not start server");
        }
        wait_events[i] = event_handle;

        let pipe_handle = CreateNamedPipeW(
            pipe_path.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE,
            MAX_CLIENT_COUNT as _,
            PIPE_BUFFER_LEN as _,
            PIPE_BUFFER_LEN as _,
            0,
            std::ptr::null_mut(),
        );
        if pipe_handle == INVALID_HANDLE_VALUE {
            panic!("could not start server");
        }

        pipes[i].handle = pipe_handle;
        pipes[i].overlapped.hEvent = event_handle;
    }
}

unsafe fn try_run_client(pipe_path: &[u16]) -> bool {
    let pipe_handle = CreateFileW(
        pipe_path.as_ptr(),
        GENERIC_READ | GENERIC_WRITE,
        0,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        0,
        NULL,
    );
    if pipe_handle == INVALID_HANDLE_VALUE {
        return false;
    }

    let mut mode = PIPE_READMODE_MESSAGE;
    if SetNamedPipeHandleState(
        pipe_handle,
        &mut mode,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    ) == FALSE
    {
        panic!("could not connect to server");
    }

    let input_handle = GetStdHandle(STD_INPUT_HANDLE);
    let output_handle = GetStdHandle(STD_OUTPUT_HANDLE);

    let mut original_input_mode = DWORD::default();
    if GetConsoleMode(input_handle, &mut original_input_mode) == FALSE {
        panic!("could not retrieve original console input mode");
    }
    if SetConsoleMode(input_handle, ENABLE_WINDOW_INPUT) == FALSE {
        panic!("could not set console input mode");
    }

    let mut original_output_mode = DWORD::default();
    if GetConsoleMode(output_handle, &mut original_output_mode) == FALSE {
        panic!("could not retrieve original console output mode");
    }
    if SetConsoleMode(
        output_handle,
        ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ) == FALSE
    {
        panic!("could not set console output mode");
    }

    let event_buffer = &mut [INPUT_RECORD::default(); 32][..];

    let waiting_handles_len = 1;
    let waiting_handles = &mut [INVALID_HANDLE_VALUE; 1][..];
    waiting_handles[0] = input_handle;

    'main_loop: loop {
        let wait_result = WaitForMultipleObjects(
            waiting_handles_len,
            waiting_handles.as_mut_ptr(),
            FALSE,
            INFINITE,
        );
        if wait_result == WAIT_FAILED {
            panic!("failed to wait on events");
        }
        if wait_result == WAIT_TIMEOUT {
            continue;
        }
        if wait_result < WAIT_OBJECT_0 {
            continue;
        }
        let waiting_handle_index = wait_result - WAIT_OBJECT_0;
        if waiting_handle_index >= waiting_handles.len() as _ {
            continue;
        }

        match waiting_handle_index {
            0 => {
                let mut event_count: DWORD = 0;
                if ReadConsoleInputW(
                    input_handle,
                    event_buffer.as_mut_ptr(),
                    event_buffer.len() as _,
                    &mut event_count,
                ) == FALSE
                {
                    panic!("could not read console events");
                }

                for i in 0..event_count {
                    let event = event_buffer[i as usize];
                    match event.EventType {
                        KEY_EVENT => {
                            let event = event.Event.KeyEvent();
                            if event.bKeyDown == FALSE {
                                continue;
                            }

                            let control_key_state = event.dwControlKeyState;
                            let keycode = event.wVirtualKeyCode as i32;
                            let repeat_count = event.wRepeatCount as usize;

                            const CHAR_A: i32 = b'A' as _;
                            const CHAR_Z: i32 = b'Z' as _;
                            let key = match keycode {
                                VK_BACK => Key::Backspace,
                                VK_RETURN => Key::Enter,
                                VK_LEFT => Key::Left,
                                VK_RIGHT => Key::Right,
                                VK_UP => Key::Up,
                                VK_DOWN => Key::Down,
                                VK_HOME => Key::Home,
                                VK_END => Key::End,
                                VK_PRIOR => Key::PageUp,
                                VK_NEXT => Key::PageDown,
                                VK_TAB => Key::Tab,
                                VK_DELETE => Key::Delete,
                                VK_F1..=VK_F24 => Key::F((keycode - VK_F1 + 1) as _),
                                VK_ESCAPE => Key::Esc,
                                CHAR_A..=CHAR_Z => {
                                    const ALT_PRESSED_MASK: DWORD =
                                        LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED;
                                    const CTRL_PRESSED_MASK: DWORD =
                                        LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;

                                    let c = keycode as u8;
                                    if control_key_state & ALT_PRESSED_MASK != 0 {
                                        Key::Alt(c.to_ascii_lowercase() as _)
                                    } else if control_key_state & CTRL_PRESSED_MASK != 0 {
                                        Key::Ctrl(c.to_ascii_lowercase() as _)
                                    } else if control_key_state & SHIFT_PRESSED != 0 {
                                        Key::Char(c as _)
                                    } else {
                                        Key::Char(c.to_ascii_lowercase() as _)
                                    }
                                }
                                _ => {
                                    let c = *(event.uChar.AsciiChar()) as u8;
                                    if !c.is_ascii_graphic() {
                                        continue;
                                    }

                                    Key::Char(c as _)
                                }
                            };

                            println!("key {} * {}", key, repeat_count);

                            if let Key::Esc = key {
                                break 'main_loop;
                            }
                        }
                        WINDOW_BUFFER_SIZE_EVENT => {
                            let size = event.Event.WindowBufferSizeEvent().dwSize;
                            let x = size.X as u16;
                            let y = size.Y as u16;
                            println!("window resized to {}, {}", x, y);
                        }
                        _ => (),
                    }
                }
            }
            _ => (),
        }
    }

    SetConsoleMode(input_handle, original_input_mode);
    SetConsoleMode(output_handle, original_output_mode);
    true
}
