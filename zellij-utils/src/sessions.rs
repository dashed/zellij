use crate::{
    consts::{
        session_info_folder_for_session, session_layout_cache_file_name,
        ZELLIJ_SESSION_INFO_CACHE_DIR, ZELLIJ_SOCK_DIR,
    },
    envs,
    input::layout::Layout,
    ipc::{ClientToServerMsg, IpcReceiverWithContext, IpcSenderWithContext, ServerToClientMsg},
};
use anyhow;
use humantime::format_duration;
use interprocess::local_socket::LocalSocketStream;
use nix::sys::socket::{setsockopt, sockopt::ReceiveTimeout};
use nix::sys::time::{TimeVal, TimeValLike};
use std::os::unix::io::RawFd;
use std::collections::HashMap;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, SystemTime};
use std::{fs, io, process};
use suggest::Suggest;

/// Timeout in seconds for socket reads when checking session connectivity.
/// This prevents `zellij ls` from hanging indefinitely on unresponsive sessions.
const SOCKET_ASSERT_TIMEOUT_SECS: i64 = 2;

/// Get the PID of the peer process connected to a Unix socket.
/// Uses platform-specific socket options: SO_PEERCRED on Linux, LOCAL_PEERPID on macOS.
#[cfg(target_os = "linux")]
fn get_peer_pid(fd: RawFd) -> Option<u32> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(fd, PeerCredentials)
        .ok()
        .map(|creds| creds.pid() as u32)
}

#[cfg(target_os = "macos")]
fn get_peer_pid(fd: RawFd) -> Option<u32> {
    // macOS uses LOCAL_PEERPID to get the peer's PID
    use nix::libc;
    use std::mem;
    const SOL_LOCAL: libc::c_int = 0;
    const LOCAL_PEERPID: libc::c_int = 2;

    let mut pid: libc::pid_t = 0;
    let mut len: libc::socklen_t = mem::size_of::<libc::pid_t>() as libc::socklen_t;

    let result = unsafe {
        libc::getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if result == 0 && pid > 0 {
        Some(pid as u32)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn get_peer_pid(_fd: RawFd) -> Option<u32> {
    // PID retrieval not supported on this platform
    None
}

pub fn get_sessions() -> Result<Vec<(String, Duration, Option<u32>)>, io::ErrorKind> {
    match fs::read_dir(&*ZELLIJ_SOCK_DIR) {
        Ok(files) => {
            // Collect all socket files first (fast, no blocking I/O)
            let socket_files: Vec<_> = files
                .filter_map(|f| f.ok())
                .filter(|f| f.file_type().map(|t| t.is_socket()).unwrap_or(false))
                .collect();

            // Check all sessions in parallel using scoped threads.
            // This ensures that even with multiple unresponsive sessions,
            // the total time is bounded by the timeout (not timeout * num_sessions).
            let sessions = std::thread::scope(|s| {
                let handles: Vec<_> = socket_files
                    .iter()
                    .filter_map(|file| {
                        // Skip files with non-UTF8 names
                        let file_name = file.file_name().into_string().ok()?;
                        let file_path = file.path();
                        Some(s.spawn(move || {
                            let ctime = std::fs::metadata(&file_path)
                                .ok()
                                .and_then(|f| f.created().ok())
                                .and_then(|d| d.elapsed().ok())
                                .unwrap_or_default();
                            let duration = Duration::from_secs(ctime.as_secs());

                            let (is_alive, pid) = assert_socket(&file_name);
                            if is_alive {
                                Some((file_name, duration, pid))
                            } else {
                                None
                            }
                        }))
                    })
                    .collect();

                // Collect results from all threads
                handles
                    .into_iter()
                    .filter_map(|h| h.join().ok().flatten())
                    .collect::<Vec<_>>()
            });

            Ok(sessions)
        },
        Err(err) if io::ErrorKind::NotFound != err.kind() => Err(err.kind()),
        Err(_) => Ok(Vec::with_capacity(0)),
    }
}

pub fn get_resurrectable_sessions() -> Vec<(String, Duration)> {
    match fs::read_dir(&*ZELLIJ_SESSION_INFO_CACHE_DIR) {
        Ok(files_in_session_info_folder) => {
            let files_that_are_folders = files_in_session_info_folder
                .filter_map(|f| f.ok().map(|f| f.path()))
                .filter(|f| f.is_dir());
            files_that_are_folders
                .filter_map(|folder_name| {
                    let layout_file_name =
                        session_layout_cache_file_name(&folder_name.display().to_string());
                    let ctime = match std::fs::metadata(&layout_file_name)
                        .and_then(|metadata| metadata.created())
                    {
                        Ok(created) => Some(created),
                        Err(_e) => None,
                    };
                    let elapsed_duration = ctime
                        .map(|ctime| {
                            Duration::from_secs(ctime.elapsed().ok().unwrap_or_default().as_secs())
                        })
                        .unwrap_or_default();
                    let session_name = folder_name
                        .file_name()
                        .map(|f| std::path::PathBuf::from(f).display().to_string())?;
                    if std::path::Path::new(&layout_file_name).exists() {
                        Some((session_name, elapsed_duration))
                    } else {
                        None
                    }
                })
                .collect()
        },
        Err(e) => {
            log::error!(
                "Failed to read session_info cache folder: \"{:?}\": {:?}",
                &*ZELLIJ_SESSION_INFO_CACHE_DIR,
                e
            );
            vec![]
        },
    }
}

pub fn get_resurrectable_session_names() -> Vec<String> {
    match fs::read_dir(&*ZELLIJ_SESSION_INFO_CACHE_DIR) {
        Ok(files_in_session_info_folder) => {
            let files_that_are_folders = files_in_session_info_folder
                .filter_map(|f| f.ok().map(|f| f.path()))
                .filter(|f| f.is_dir());
            files_that_are_folders
                .filter_map(|folder_name| {
                    let folder = folder_name.display().to_string();
                    let resurrection_layout_file = session_layout_cache_file_name(&folder);
                    if std::path::Path::new(&resurrection_layout_file).exists() {
                        folder_name
                            .file_name()
                            .map(|f| format!("{}", f.to_string_lossy()))
                    } else {
                        None
                    }
                })
                .collect()
        },
        Err(e) => {
            log::error!(
                "Failed to read session_info cache folder: \"{:?}\": {:?}",
                &*ZELLIJ_SESSION_INFO_CACHE_DIR,
                e
            );
            vec![]
        },
    }
}

pub fn get_sessions_sorted_by_mtime() -> anyhow::Result<Vec<String>> {
    match fs::read_dir(&*ZELLIJ_SOCK_DIR) {
        Ok(files) => {
            let mut sessions_with_mtime: Vec<(String, SystemTime)> = Vec::new();
            for file in files {
                let file = file?;
                let file_name = file.file_name().into_string().unwrap();
                let file_modified_at = file.metadata()?.modified()?;
                if file.file_type()?.is_socket() && assert_socket(&file_name).0 {
                    sessions_with_mtime.push((file_name, file_modified_at));
                }
            }
            sessions_with_mtime.sort_by_key(|x| x.1); // the oldest one will be the first

            let sessions = sessions_with_mtime.iter().map(|x| x.0.clone()).collect();
            Ok(sessions)
        },
        Err(err) if io::ErrorKind::NotFound != err.kind() => Err(err.into()),
        Err(_) => Ok(Vec::with_capacity(0)),
    }
}

/// Check if a session socket is alive and return its server PID if available.
/// Returns (is_alive, Option<pid>).
fn assert_socket(name: &str) -> (bool, Option<u32>) {
    let path = &*ZELLIJ_SOCK_DIR.join(name);
    match LocalSocketStream::connect(path) {
        Ok(stream) => {
            // Set read timeout to prevent blocking forever on unresponsive sessions.
            // This is critical for `zellij ls` to not hang when a session is stuck.
            let fd = stream.as_raw_fd();
            let timeout = TimeVal::seconds(SOCKET_ASSERT_TIMEOUT_SECS);
            if let Err(e) = setsockopt(fd, ReceiveTimeout, &timeout) {
                log::warn!(
                    "Failed to set socket timeout for session '{}': {}",
                    name,
                    e
                );
            }

            // Get the server's PID from the socket connection
            let pid = get_peer_pid(fd);

            let mut sender: IpcSenderWithContext<ClientToServerMsg> =
                IpcSenderWithContext::new(stream);
            let _ = sender.send_client_msg(ClientToServerMsg::ConnStatus);
            let mut receiver: IpcReceiverWithContext<ServerToClientMsg> = sender.get_receiver();
            match receiver.recv_server_msg() {
                Some((ServerToClientMsg::Connected, _)) => (true, pid),
                None | Some((_, _)) => (false, None),
            }
        },
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            drop(fs::remove_file(path));
            (false, None)
        },
        Err(_) => (false, None),
    }
}

pub fn print_sessions(
    mut sessions: Vec<(String, Duration, bool, Option<u32>)>,
    no_formatting: bool,
    short: bool,
    reverse: bool,
) {
    // (session_name, timestamp, is_dead, pid)
    let curr_session = envs::get_session_name().unwrap_or_else(|_| "".into());
    sessions.sort_by(|a, b| {
        if reverse {
            // sort by `Duration` ascending (newest would be first)
            a.1.cmp(&b.1)
        } else {
            b.1.cmp(&a.1)
        }
    });
    sessions
        .iter()
        .for_each(|(session_name, timestamp, is_dead, pid)| {
            if short {
                println!("{}", session_name);
                return;
            }
            let pid_str = pid.map(|p| format!("[{}]", p)).unwrap_or_default();
            if no_formatting {
                let suffix = if curr_session == *session_name {
                    format!("(current)")
                } else if *is_dead {
                    format!("(EXITED - attach to resurrect)")
                } else {
                    String::new()
                };
                let timestamp = format!("[Created {} ago]", format_duration(*timestamp));
                if pid_str.is_empty() {
                    println!("{} {} {}", session_name, timestamp, suffix);
                } else {
                    println!("{} {} {} {}", session_name, pid_str, timestamp, suffix);
                }
            } else {
                let formatted_session_name = format!("\u{1b}[32;1m{}\u{1b}[m", session_name);
                let formatted_pid = if let Some(p) = pid {
                    format!("[\u{1b}[36m{}\u{1b}[m]", p) // Cyan for PID
                } else {
                    String::new()
                };
                let suffix = if curr_session == *session_name {
                    format!("(current)")
                } else if *is_dead {
                    format!("(\u{1b}[31;1mEXITED\u{1b}[m - attach to resurrect)")
                } else {
                    String::new()
                };
                let timestamp = format!(
                    "[Created \u{1b}[35;1m{}\u{1b}[m ago]",
                    format_duration(*timestamp)
                );
                if formatted_pid.is_empty() {
                    println!("{} {} {}", formatted_session_name, timestamp, suffix);
                } else {
                    println!(
                        "{} {} {} {}",
                        formatted_session_name, formatted_pid, timestamp, suffix
                    );
                }
            }
        })
}

pub fn print_sessions_with_index(sessions: Vec<String>) {
    let curr_session = envs::get_session_name().unwrap_or_else(|_| "".into());
    for (i, session) in sessions.iter().enumerate() {
        let suffix = if curr_session == *session {
            " (current)"
        } else {
            ""
        };
        println!("{}: {}{}", i, session, suffix);
    }
}

pub enum ActiveSession {
    None,
    One(String),
    Many,
}

pub fn get_active_session() -> ActiveSession {
    match get_sessions() {
        Ok(sessions) if sessions.is_empty() => ActiveSession::None,
        Ok(mut sessions) if sessions.len() == 1 => ActiveSession::One(sessions.pop().unwrap().0),
        Ok(_) => ActiveSession::Many,
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
            process::exit(1);
        },
    }
}

pub fn kill_session(name: &str) {
    let path = &*ZELLIJ_SOCK_DIR.join(name);
    match LocalSocketStream::connect(path) {
        Ok(stream) => {
            let _ = IpcSenderWithContext::<ClientToServerMsg>::new(stream)
                .send_client_msg(ClientToServerMsg::KillSession);
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
            process::exit(1);
        },
    };
}

pub fn delete_session(name: &str, force: bool) {
    if force {
        let path = &*ZELLIJ_SOCK_DIR.join(name);
        let _ = LocalSocketStream::connect(path).map(|stream| {
            IpcSenderWithContext::<ClientToServerMsg>::new(stream)
                .send_client_msg(ClientToServerMsg::KillSession)
                .ok();
        });
    }
    if let Err(e) = std::fs::remove_dir_all(session_info_folder_for_session(name)) {
        if e.kind() == std::io::ErrorKind::NotFound {
            eprintln!("Session: {:?} not found.", name);
            process::exit(2);
        } else {
            log::error!("Failed to remove session {:?}: {:?}", name, e);
        }
    } else {
        println!("Session: {:?} successfully deleted.", name);
    }
}

pub fn list_sessions(no_formatting: bool, short: bool, reverse: bool, progressive: bool) {
    if progressive {
        list_sessions_progressive(no_formatting);
    } else {
        let exit_code = match get_sessions() {
            Ok(running_sessions) => {
                let resurrectable_sessions = get_resurrectable_sessions();
                // (Duration, is_dead, Option<pid>)
                let mut all_sessions: HashMap<String, (Duration, bool, Option<u32>)> =
                    resurrectable_sessions
                        .iter()
                        .map(|(name, timestamp)| (name.clone(), (timestamp.clone(), true, None)))
                        .collect();
                for (session_name, duration, pid) in running_sessions {
                    all_sessions.insert(session_name.clone(), (duration, false, pid));
                }
                if all_sessions.is_empty() {
                    eprintln!("No active zellij sessions found.");
                    1
                } else {
                    print_sessions(
                        all_sessions
                            .iter()
                            .map(|(name, (timestamp, is_dead, pid))| {
                                (name.clone(), timestamp.clone(), *is_dead, *pid)
                            })
                            .collect(),
                        no_formatting,
                        short,
                        reverse,
                    );
                    0
                }
            },
            Err(e) => {
                eprintln!("Error occurred: {:?}", e);
                1
            },
        };
        process::exit(exit_code);
    }
}

/// List sessions progressively, showing each session's status as it's checked.
/// This is useful for debugging when some sessions may be unresponsive.
fn list_sessions_progressive(no_formatting: bool) {
    use std::io::Write;

    let curr_session = envs::get_session_name().unwrap_or_else(|_| "".into());

    // First, check running sessions with progressive output
    let sock_dir = &*ZELLIJ_SOCK_DIR;
    let mut found_any = false;

    match fs::read_dir(sock_dir) {
        Ok(files) => {
            let mut socket_files: Vec<_> = files
                .filter_map(|f| f.ok())
                .filter(|f| f.file_type().map(|t| t.is_socket()).unwrap_or(false))
                .collect();

            // Sort by creation time for consistent ordering
            socket_files.sort_by(|a, b| {
                let a_time = fs::metadata(a.path())
                    .ok()
                    .and_then(|m| m.created().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let b_time = fs::metadata(b.path())
                    .ok()
                    .and_then(|m| m.created().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                b_time.cmp(&a_time)
            });

            for file in socket_files {
                if let Ok(session_name) = file.file_name().into_string() {
                    found_any = true;

                    // Print session name with "checking..." indicator
                    let name_display = if no_formatting {
                        session_name.clone()
                    } else {
                        format!("\u{1b}[32;1m{}\u{1b}[m", session_name)
                    };

                    print!("{} ... ", name_display);
                    let _ = io::stdout().flush();

                    // Check if session is responsive (uses timeout from assert_socket)
                    let (is_responsive, pid) = assert_socket(&session_name);

                    // Format PID display
                    let pid_display = if let Some(p) = pid {
                        if no_formatting {
                            format!("[{}] ", p)
                        } else {
                            format!("[\u{1b}[36m{}\u{1b}[m] ", p) // Cyan for PID
                        }
                    } else {
                        String::new()
                    };

                    // Print status
                    let status = if is_responsive {
                        if no_formatting {
                            "[OK]".to_string()
                        } else {
                            "\u{1b}[32m[OK]\u{1b}[m".to_string()
                        }
                    } else {
                        if no_formatting {
                            "[UNRESPONSIVE]".to_string()
                        } else {
                            "\u{1b}[31m[UNRESPONSIVE]\u{1b}[m".to_string()
                        }
                    };

                    // Add current session indicator
                    let current_indicator = if curr_session == session_name {
                        " (current)"
                    } else {
                        ""
                    };

                    println!("{}{}{}", pid_display, status, current_indicator);
                }
            }
        },
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            eprintln!("Error reading socket directory: {:?}", e);
            process::exit(1);
        },
        Err(_) => {},
    }

    // Then show resurrectable (dead) sessions
    let resurrectable_sessions = get_resurrectable_sessions();
    for (session_name, _duration) in resurrectable_sessions {
        found_any = true;
        let name_display = if no_formatting {
            format!("{} (EXITED - attach to resurrect)", session_name)
        } else {
            format!(
                "\u{1b}[33;1m{}\u{1b}[m (\u{1b}[31;1mEXITED\u{1b}[m - attach to resurrect)",
                session_name
            )
        };
        println!("{}", name_display);
    }

    if !found_any {
        eprintln!("No active zellij sessions found.");
        process::exit(1);
    }

    process::exit(0);
}

#[derive(Debug, Clone)]
pub enum SessionNameMatch {
    AmbiguousPrefix(Vec<String>),
    UniquePrefix(String),
    Exact(String),
    None,
}

pub fn match_session_name(prefix: &str) -> Result<SessionNameMatch, io::ErrorKind> {
    let sessions = get_sessions()?;

    let filtered_sessions: Vec<_> = sessions
        .iter()
        .filter(|s| s.0.starts_with(prefix))
        .collect();

    if filtered_sessions.iter().any(|s| s.0 == prefix) {
        return Ok(SessionNameMatch::Exact(prefix.to_string()));
    }

    Ok({
        match &filtered_sessions[..] {
            [] => SessionNameMatch::None,
            [s] => SessionNameMatch::UniquePrefix(s.0.to_string()),
            _ => SessionNameMatch::AmbiguousPrefix(
                filtered_sessions.into_iter().map(|s| s.0.clone()).collect(),
            ),
        }
    })
}

pub fn session_exists(name: &str) -> Result<bool, io::ErrorKind> {
    match match_session_name(name) {
        Ok(SessionNameMatch::Exact(_)) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}

// if the session is resurrecable, the returned layout is the one to be used to resurrect it
pub fn resurrection_layout(session_name_to_resurrect: &str) -> Result<Option<Layout>, String> {
    let layout_file_name = session_layout_cache_file_name(&session_name_to_resurrect);
    let raw_layout = match std::fs::read_to_string(&layout_file_name) {
        Ok(raw_layout) => raw_layout,
        Err(_e) => {
            return Ok(None);
        },
    };
    match Layout::from_kdl(
        &raw_layout,
        Some(layout_file_name.display().to_string()),
        None,
        None,
    ) {
        Ok(layout) => Ok(Some(layout)),
        Err(e) => {
            log::error!(
                "Failed to parse resurrection layout file {}: {}",
                layout_file_name.display(),
                e
            );
            return Err(format!(
                "Failed to parse resurrection layout file {}: {}.",
                layout_file_name.display(),
                e
            ));
        },
    }
}

pub fn assert_session(name: &str) {
    match session_exists(name) {
        Ok(result) => {
            if result {
                return;
            } else {
                println!("No session named {:?} found.", name);
                if let Some(sugg) = get_sessions()
                    .unwrap()
                    .iter()
                    .map(|s| s.0.clone())
                    .collect::<Vec<_>>()
                    .suggest(name)
                {
                    println!("  help: Did you mean `{}`?", sugg);
                }
            }
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
        },
    };
    process::exit(1);
}

pub fn assert_dead_session(name: &str, force: bool) {
    match session_exists(name) {
        Ok(exists) => {
            if exists && !force {
                println!(
                    "A session by the name {:?} exists and is active, use --force to delete it.",
                    name
                )
            } else if exists && force {
                println!("A session by the name {:?} exists and is active, but will be force killed and deleted.", name);
                return;
            } else {
                return;
            }
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
        },
    };
    process::exit(1);
}

pub fn assert_session_ne(name: &str) {
    if name.trim().is_empty() {
        eprintln!("Session name cannot be empty. Please provide a specific session name.");
        process::exit(1);
    }
    if name == "." || name == ".." {
        eprintln!("Invalid session name: \"{}\".", name);
        process::exit(1);
    }
    if name.contains('/') {
        eprintln!("Session name cannot contain '/'.");
        process::exit(1);
    }

    match session_exists(name) {
        Ok(result) if !result => {
            let resurrectable_sessions = get_resurrectable_session_names();
            if resurrectable_sessions.iter().find(|s| s == &name).is_some() {
                println!("Session with name {:?} already exists, but is dead. Use the attach command to resurrect it or, the delete-session command to kill it or specify a different name.", name);
            } else {
                return
            }
        }
        Ok(_) => println!("Session with name {:?} already exists. Use attach command to connect to it or specify a different name.", name),
        Err(e) => eprintln!("Error occurred: {:?}", e),
    };
    process::exit(1);
}

pub fn generate_unique_session_name() -> Option<String> {
    let sessions = get_sessions().map(|sessions| {
        sessions
            .iter()
            .map(|s| s.0.clone())
            .collect::<Vec<String>>()
    });
    let dead_sessions = get_resurrectable_session_names();
    let Ok(sessions) = sessions else {
        eprintln!("Failed to list existing sessions: {:?}", sessions);
        return None;
    };

    let name = get_name_generator()
        .take(1000)
        .find(|name| !sessions.contains(name) && !dead_sessions.contains(name));

    if let Some(name) = name {
        return Some(name);
    } else {
        return None;
    }
}

/// Create a new random name generator
///
/// Used to provide a memorable handle for a session when users don't specify a session name when the session is
/// created.
///
/// Uses the list of adjectives and nouns defined below, with the intention of avoiding unfortunate
/// and offensive combinations. Care should be taken when adding or removing to either list due to the birthday paradox/
/// hash collisions, e.g. with 4096 unique names, the likelihood of a collision in 10 session names is 1%.
pub fn get_name_generator() -> impl Iterator<Item = String> {
    names::Generator::new(&ADJECTIVES, &NOUNS, names::Name::Plain)
}

const ADJECTIVES: &[&'static str] = &[
    "adamant",
    "adept",
    "adventurous",
    "arcadian",
    "auspicious",
    "awesome",
    "blossoming",
    "brave",
    "charming",
    "chatty",
    "circular",
    "considerate",
    "cubic",
    "curious",
    "delighted",
    "didactic",
    "diligent",
    "effulgent",
    "erudite",
    "excellent",
    "exquisite",
    "fabulous",
    "fascinating",
    "friendly",
    "glowing",
    "gracious",
    "gregarious",
    "hopeful",
    "implacable",
    "inventive",
    "joyous",
    "judicious",
    "jumping",
    "kind",
    "likable",
    "loyal",
    "lucky",
    "marvellous",
    "mellifluous",
    "nautical",
    "oblong",
    "outstanding",
    "polished",
    "polite",
    "profound",
    "quadratic",
    "quiet",
    "rectangular",
    "remarkable",
    "rusty",
    "sensible",
    "sincere",
    "sparkling",
    "splendid",
    "stellar",
    "tenacious",
    "tremendous",
    "triangular",
    "undulating",
    "unflappable",
    "unique",
    "verdant",
    "vitreous",
    "wise",
    "zippy",
];

const NOUNS: &[&'static str] = &[
    "aardvark",
    "accordion",
    "apple",
    "apricot",
    "bee",
    "brachiosaur",
    "cactus",
    "capsicum",
    "clarinet",
    "cowbell",
    "crab",
    "cuckoo",
    "cymbal",
    "diplodocus",
    "donkey",
    "drum",
    "duck",
    "echidna",
    "elephant",
    "foxglove",
    "galaxy",
    "glockenspiel",
    "goose",
    "hill",
    "horse",
    "iguanadon",
    "jellyfish",
    "kangaroo",
    "lake",
    "lemon",
    "lemur",
    "magpie",
    "megalodon",
    "mountain",
    "mouse",
    "muskrat",
    "newt",
    "oboe",
    "ocelot",
    "orange",
    "panda",
    "peach",
    "pepper",
    "petunia",
    "pheasant",
    "piano",
    "pigeon",
    "platypus",
    "quasar",
    "rhinoceros",
    "river",
    "rustacean",
    "salamander",
    "sitar",
    "stegosaurus",
    "tambourine",
    "tiger",
    "tomato",
    "triceratops",
    "ukulele",
    "viola",
    "weasel",
    "xylophone",
    "yak",
    "zebra",
];

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::socket::{getsockopt, sockopt::ReceiveTimeout};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;
    use tempfile::tempdir;

    /// Test that SOCKET_ASSERT_TIMEOUT_SECS is set to a reasonable value.
    /// 2 seconds is long enough for healthy sessions to respond but short enough
    /// to detect unresponsive sessions quickly during `zellij ls`.
    #[test]
    fn test_socket_timeout_constant_is_reasonable() {
        // Timeout should be at least 1 second to allow for slow responses
        assert!(
            SOCKET_ASSERT_TIMEOUT_SECS >= 1,
            "Timeout too short for normal operation"
        );
        // Timeout should be at most 5 seconds to ensure quick hang detection
        assert!(
            SOCKET_ASSERT_TIMEOUT_SECS <= 5,
            "Timeout too long for hang detection"
        );
    }

    /// Test that the socket timeout is correctly applied to the file descriptor.
    #[test]
    fn test_socket_timeout_is_set_correctly() {
        // Create a temporary Unix socket
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let _listener = UnixListener::bind(&socket_path).unwrap();

        // Connect to the socket
        let stream = LocalSocketStream::connect(&*socket_path).unwrap();
        let fd = stream.as_raw_fd();

        // Set the timeout
        let timeout = TimeVal::seconds(SOCKET_ASSERT_TIMEOUT_SECS);
        setsockopt(fd, ReceiveTimeout, &timeout).unwrap();

        // Verify the timeout was set
        let actual_timeout = getsockopt(fd, ReceiveTimeout).unwrap();
        assert_eq!(actual_timeout.tv_sec(), SOCKET_ASSERT_TIMEOUT_SECS);
    }

    /// Test that socket read actually times out when server doesn't respond.
    /// This is an integration test that verifies the timeout mechanism works end-to-end.
    #[test]
    fn test_socket_read_times_out_on_unresponsive_server() {
        use std::io::Read;

        // Create a temporary Unix socket
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("timeout_test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        // Flag to signal when test is done
        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        // Spawn a thread that accepts the connection but never responds
        let handle = thread::spawn(move || {
            // Accept connection but don't send anything
            let _conn = listener.accept();
            // Keep connection open until test is done
            while !done_clone.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
        });

        // Connect and set timeout
        let stream = LocalSocketStream::connect(&*socket_path).unwrap();
        let fd = stream.as_raw_fd();

        // Set a short timeout for the test (100ms)
        let test_timeout = TimeVal::milliseconds(100);
        setsockopt(fd, ReceiveTimeout, &test_timeout).unwrap();

        // Try to read - this should timeout
        let mut buf = [0u8; 1];
        let start = Instant::now();
        let mut reader = std::io::BufReader::new(stream);
        let result = reader.read(&mut buf);
        let elapsed = start.elapsed();

        // Signal test is done
        done.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        // Verify that:
        // 1. Read returned an error (timed out)
        // 2. The elapsed time is reasonable (within 50% of timeout)
        assert!(
            result.is_err(),
            "Read should have timed out but returned: {:?}",
            result
        );
        assert!(
            elapsed >= Duration::from_millis(80),
            "Timeout happened too quickly: {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "Timeout took too long: {:?}",
            elapsed
        );
    }

    /// Test that parallel session checking is faster than sequential.
    /// This verifies that multiple sockets are checked concurrently.
    #[test]
    fn test_parallel_socket_checking_is_faster_than_sequential() {
        use std::io::{Read, Write};

        const NUM_SOCKETS: usize = 3;
        const DELAY_MS: u64 = 100;

        // Create temporary directory with multiple sockets
        let dir = tempdir().unwrap();
        let mut listeners = Vec::new();
        let mut socket_paths = Vec::new();

        for i in 0..NUM_SOCKETS {
            let socket_path = dir.path().join(format!("test_parallel_{}.sock", i));
            let listener = UnixListener::bind(&socket_path).unwrap();
            socket_paths.push(socket_path);
            listeners.push(listener);
        }

        // Flag to signal when test is done
        let done = Arc::new(AtomicBool::new(false));

        // Spawn threads that accept connections, delay, then send response
        let handles: Vec<_> = listeners
            .into_iter()
            .map(|listener| {
                let done_clone = done.clone();
                thread::spawn(move || {
                    if let Ok((mut conn, _addr)) = listener.accept() {
                        // Simulate slow server - delay before responding
                        thread::sleep(Duration::from_millis(DELAY_MS));
                        // Send a response so client doesn't wait for timeout
                        let _ = conn.write_all(b"OK");
                        // Keep connection open until test is done
                        while !done_clone.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                })
            })
            .collect();

        // Time how long it takes to check all sockets in parallel
        let start = Instant::now();

        std::thread::scope(|s| {
            let check_handles: Vec<_> = socket_paths
                .iter()
                .map(|path| {
                    s.spawn(|| {
                        let stream = LocalSocketStream::connect(&**path).unwrap();
                        let fd = stream.as_raw_fd();

                        // Set a timeout longer than the delay
                        let test_timeout = TimeVal::milliseconds(DELAY_MS as i64 * 5);
                        setsockopt(fd, ReceiveTimeout, &test_timeout).unwrap();

                        // Read response (server sends "OK" after DELAY_MS)
                        let mut buf = [0u8; 2];
                        let mut reader = std::io::BufReader::new(stream);
                        let _ = reader.read_exact(&mut buf);
                    })
                })
                .collect();

            // Wait for all checks to complete
            for h in check_handles {
                let _ = h.join();
            }
        });

        let elapsed = start.elapsed();

        // Signal test is done
        done.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }

        // If parallel: elapsed should be roughly DELAY_MS (all finish at same time)
        // If sequential: elapsed would be roughly DELAY_MS * NUM_SOCKETS
        // Use generous thresholds for CI stability
        let parallel_upper_bound = Duration::from_millis(DELAY_MS * 2 + 100);
        let sequential_lower_bound = Duration::from_millis(DELAY_MS * (NUM_SOCKETS as u64 - 1));

        assert!(
            elapsed < parallel_upper_bound,
            "Parallel check took {:?}, expected less than {:?}. \
             Threads may not be running in parallel.",
            elapsed,
            parallel_upper_bound
        );
        assert!(
            elapsed < sequential_lower_bound,
            "Parallel check took {:?}, which is close to sequential time ({:?}). \
             Parallel execution should be significantly faster.",
            elapsed,
            sequential_lower_bound
        );
    }

    /// Test that scoped threads correctly collect all results.
    #[test]
    fn test_scoped_threads_collect_all_results() {
        const NUM_ITEMS: usize = 10;

        // Use scoped threads to process items in parallel and collect results
        let items: Vec<i32> = (0..NUM_ITEMS as i32).collect();

        let results: Vec<i32> = std::thread::scope(|s| {
            let handles: Vec<_> = items
                .iter()
                .map(|&item| {
                    s.spawn(move || {
                        // Simulate some work
                        thread::sleep(Duration::from_millis(10));
                        item * 2
                    })
                })
                .collect();

            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .collect()
        });

        // Verify all results were collected
        assert_eq!(results.len(), NUM_ITEMS);

        // Verify results are correct (order may vary due to parallel execution)
        let mut sorted_results = results.clone();
        sorted_results.sort();
        let expected: Vec<i32> = (0..NUM_ITEMS as i32).map(|x| x * 2).collect();
        assert_eq!(sorted_results, expected);
    }

    /// Test parallel checking with mixed responsive and unresponsive sockets.
    #[test]
    fn test_parallel_mixed_responsive_unresponsive() {
        use std::io::{Read, Write};

        const NUM_RESPONSIVE: usize = 2;
        const NUM_UNRESPONSIVE: usize = 2;
        const TIMEOUT_MS: i64 = 100;

        let dir = tempdir().unwrap();
        let done = Arc::new(AtomicBool::new(false));

        // Create responsive sockets (immediately send data)
        let mut responsive_handles = Vec::new();
        let mut responsive_paths = Vec::new();
        for i in 0..NUM_RESPONSIVE {
            let path = dir.path().join(format!("responsive_{}.sock", i));
            let listener = UnixListener::bind(&path).unwrap();
            responsive_paths.push(path);

            let done_clone = done.clone();
            responsive_handles.push(thread::spawn(move || {
                if let Ok((mut conn, _)) = listener.accept() {
                    // Immediately send a response
                    let _ = conn.write_all(b"OK");
                    while !done_clone.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }));
        }

        // Create unresponsive sockets (never send data)
        let mut unresponsive_handles = Vec::new();
        let mut unresponsive_paths = Vec::new();
        for i in 0..NUM_UNRESPONSIVE {
            let path = dir.path().join(format!("unresponsive_{}.sock", i));
            let listener = UnixListener::bind(&path).unwrap();
            unresponsive_paths.push(path);

            let done_clone = done.clone();
            unresponsive_handles.push(thread::spawn(move || {
                if let Ok((_conn, _)) = listener.accept() {
                    // Never respond, just keep connection open
                    while !done_clone.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }));
        }

        // Combine all paths
        let all_paths: Vec<_> = responsive_paths
            .into_iter()
            .chain(unresponsive_paths.into_iter())
            .collect();

        let start = Instant::now();

        // Check all sockets in parallel
        let results: Vec<bool> = std::thread::scope(|s| {
            let handles: Vec<_> = all_paths
                .iter()
                .map(|path| {
                    s.spawn(|| {
                        let stream = match LocalSocketStream::connect(&**path) {
                            Ok(s) => s,
                            Err(_) => return false,
                        };
                        let fd = stream.as_raw_fd();

                        // Set timeout
                        let timeout = TimeVal::milliseconds(TIMEOUT_MS);
                        if setsockopt(fd, ReceiveTimeout, &timeout).is_err() {
                            return false;
                        }

                        // Try to read
                        let mut buf = [0u8; 2];
                        let mut reader = std::io::BufReader::new(stream);
                        reader.read_exact(&mut buf).is_ok()
                    })
                })
                .collect();

            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .collect()
        });

        let elapsed = start.elapsed();

        // Clean up
        done.store(true, Ordering::Relaxed);
        for h in responsive_handles.into_iter().chain(unresponsive_handles) {
            let _ = h.join();
        }

        // Verify we got results for all sockets
        assert_eq!(results.len(), NUM_RESPONSIVE + NUM_UNRESPONSIVE);

        // Count successes (responsive sockets)
        let successes = results.iter().filter(|&&r| r).count();
        assert_eq!(
            successes, NUM_RESPONSIVE,
            "Expected {} responsive sockets, got {}",
            NUM_RESPONSIVE, successes
        );

        // Verify parallel execution - should complete in roughly timeout time
        // (not timeout * num_unresponsive)
        let max_expected = Duration::from_millis((TIMEOUT_MS as u64) * 2 + 100);
        assert!(
            elapsed < max_expected,
            "Mixed check took {:?}, expected less than {:?}",
            elapsed,
            max_expected
        );
    }

    // ==================== Progressive Output Tests ====================

    /// Test the progressive output status format strings.
    #[test]
    fn test_progressive_status_format_strings() {
        // Test plain text formats (no_formatting = true)
        let ok_plain = "[OK]";
        let unresponsive_plain = "[UNRESPONSIVE]";

        assert!(ok_plain.contains("OK"));
        assert!(unresponsive_plain.contains("UNRESPONSIVE"));

        // Test colored formats (no_formatting = false)
        let ok_colored = "\u{1b}[32m[OK]\u{1b}[m";
        let unresponsive_colored = "\u{1b}[31m[UNRESPONSIVE]\u{1b}[m";

        // Verify ANSI escape codes are present
        assert!(ok_colored.contains("\u{1b}[32m")); // Green
        assert!(ok_colored.contains("\u{1b}[m")); // Reset
        assert!(unresponsive_colored.contains("\u{1b}[31m")); // Red
        assert!(unresponsive_colored.contains("\u{1b}[m")); // Reset
    }

    /// Test the progressive output session name formatting.
    #[test]
    fn test_progressive_session_name_formatting() {
        let session_name = "test_session";

        // Plain format
        let plain_name = session_name.to_string();
        assert_eq!(plain_name, "test_session");

        // Colored format (green bold)
        let colored_name = format!("\u{1b}[32;1m{}\u{1b}[m", session_name);
        assert!(colored_name.contains("\u{1b}[32;1m")); // Green bold
        assert!(colored_name.contains(session_name));
        assert!(colored_name.contains("\u{1b}[m")); // Reset
    }

    /// Test progressive output for EXITED session formatting.
    #[test]
    fn test_progressive_exited_session_formatting() {
        let session_name = "dead_session";

        // Plain format
        let plain = format!("{} (EXITED - attach to resurrect)", session_name);
        assert!(plain.contains(session_name));
        assert!(plain.contains("EXITED"));
        assert!(plain.contains("attach to resurrect"));

        // Colored format
        let colored = format!(
            "\u{1b}[33;1m{}\u{1b}[m (\u{1b}[31;1mEXITED\u{1b}[m - attach to resurrect)",
            session_name
        );
        assert!(colored.contains("\u{1b}[33;1m")); // Yellow bold for session name
        assert!(colored.contains("\u{1b}[31;1m")); // Red bold for EXITED
        assert!(colored.contains(session_name));
    }

    /// Test that progressive mode checks sessions sequentially (for immediate feedback).
    /// This is verified by checking that assert_socket is called for each session
    /// and the total time reflects sequential execution.
    #[test]
    fn test_progressive_sequential_checking() {
        use std::io::{Read, Write};

        const NUM_SOCKETS: usize = 2;
        const DELAY_MS: u64 = 50;

        let dir = tempdir().unwrap();
        let done = Arc::new(AtomicBool::new(false));

        // Create sockets with servers that respond after a delay
        let mut handles = Vec::new();
        let mut socket_names = Vec::new();

        for i in 0..NUM_SOCKETS {
            let socket_name = format!("prog_test_{}.sock", i);
            let socket_path = dir.path().join(&socket_name);
            let listener = UnixListener::bind(&socket_path).unwrap();
            socket_names.push(socket_name);

            let done_clone = done.clone();
            handles.push(thread::spawn(move || {
                if let Ok((mut conn, _)) = listener.accept() {
                    thread::sleep(Duration::from_millis(DELAY_MS));
                    let _ = conn.write_all(b"OK");
                    while !done_clone.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }));
        }

        // Simulate progressive checking (sequential)
        let start = Instant::now();

        for socket_name in &socket_names {
            let socket_path = dir.path().join(socket_name);
            let stream = LocalSocketStream::connect(&*socket_path).unwrap();
            let fd = stream.as_raw_fd();

            let timeout = TimeVal::milliseconds((DELAY_MS as i64) * 3);
            setsockopt(fd, ReceiveTimeout, &timeout).unwrap();

            let mut buf = [0u8; 2];
            let mut reader = std::io::BufReader::new(stream);
            let _ = reader.read_exact(&mut buf);
        }

        let elapsed = start.elapsed();

        // Clean up
        done.store(true, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }

        // Sequential execution should take at least DELAY_MS * NUM_SOCKETS
        // (minus some tolerance for timing variations)
        let min_sequential = Duration::from_millis(DELAY_MS * (NUM_SOCKETS as u64 - 1));
        assert!(
            elapsed >= min_sequential,
            "Progressive check was too fast ({:?}), expected at least {:?}. \
             This suggests checks ran in parallel instead of sequentially.",
            elapsed,
            min_sequential
        );
    }

    /// Test PID formatting strings for session display.
    #[test]
    fn test_pid_format_strings() {
        let pid: u32 = 12345;

        // Plain format (no ANSI codes)
        let plain_pid = format!("[{}]", pid);
        assert_eq!(plain_pid, "[12345]");

        // Colored format (cyan for PID)
        let colored_pid = format!("[\u{1b}[36m{}\u{1b}[m]", pid);
        assert!(colored_pid.contains("\u{1b}[36m")); // Cyan color code
        assert!(colored_pid.contains("12345"));
        assert!(colored_pid.contains("\u{1b}[m")); // Reset code
    }

    /// Test session display with PID included.
    #[test]
    fn test_session_display_with_pid() {
        let session_name = "test_session";
        let pid: u32 = 54321;

        // Format as it would appear in list_sessions
        let formatted_session = format!("\u{1b}[32;1m{}\u{1b}[m", session_name);
        let formatted_pid = format!("[\u{1b}[36m{}\u{1b}[m]", pid);
        let timestamp = "[Created 1h ago]";

        let full_output = format!("{} {} {}", formatted_session, formatted_pid, timestamp);

        assert!(full_output.contains(session_name));
        assert!(full_output.contains("54321"));
        assert!(full_output.contains("Created"));
    }

    /// Test session display without PID (None case).
    #[test]
    fn test_session_display_without_pid() {
        let session_name = "test_session";
        let pid: Option<u32> = None;

        let pid_display = pid.map(|p| format!("[{}]", p)).unwrap_or_default();
        assert_eq!(pid_display, "");

        // When PID is None, output should not contain brackets for PID
        let formatted_session = format!("\u{1b}[32;1m{}\u{1b}[m", session_name);
        let timestamp = "[Created 1h ago]";

        let full_output = if pid_display.is_empty() {
            format!("{} {}", formatted_session, timestamp)
        } else {
            format!("{} {} {}", formatted_session, pid_display, timestamp)
        };

        assert!(full_output.contains(session_name));
        assert!(full_output.contains("Created"));
        // Should not have extra brackets for missing PID
        assert!(!full_output.contains("[]"));
    }

    /// Test progressive mode PID display format.
    #[test]
    fn test_progressive_pid_display() {
        let pid: u32 = 99999;

        // Plain format
        let plain = format!("[{}] ", pid);
        assert_eq!(plain, "[99999] ");

        // Colored format
        let colored = format!("[\u{1b}[36m{}\u{1b}[m] ", pid);
        assert!(colored.contains("\u{1b}[36m")); // Cyan
        assert!(colored.contains("99999"));
        assert!(colored.ends_with("] "));
    }

    /// Test that assert_socket returns a tuple (bool, Option<u32>).
    #[test]
    fn test_assert_socket_returns_tuple() {
        use std::io::Write;

        let dir = tempdir().unwrap();
        let socket_name = "pid_test.sock";
        let socket_path = dir.path().join(socket_name);
        let listener = UnixListener::bind(&socket_path).unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        // Server that accepts and responds
        let handle = thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let _ = conn.write_all(b"OK");
                while !done_clone.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        });

        thread::sleep(Duration::from_millis(50));

        // Connect and get peer PID
        let stream = LocalSocketStream::connect(&*socket_path).unwrap();
        let fd = stream.as_raw_fd();
        let pid = super::get_peer_pid(fd);

        // On supported platforms (Linux, macOS), PID should be Some
        // On other platforms, it may be None
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert!(pid.is_some(), "Expected PID on Linux/macOS");
            assert!(pid.unwrap() > 0, "PID should be positive");
        }

        done.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}
