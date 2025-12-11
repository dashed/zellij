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
use std::collections::HashMap;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, SystemTime};
use std::{fs, io, process};
use suggest::Suggest;

/// Timeout in seconds for socket reads when checking session connectivity.
/// This prevents `zellij ls` from hanging indefinitely on unresponsive sessions.
const SOCKET_ASSERT_TIMEOUT_SECS: i64 = 2;

pub fn get_sessions() -> Result<Vec<(String, Duration)>, io::ErrorKind> {
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

                            if assert_socket(&file_name) {
                                Some((file_name, duration))
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
                if file.file_type()?.is_socket() && assert_socket(&file_name) {
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

fn assert_socket(name: &str) -> bool {
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

            let mut sender: IpcSenderWithContext<ClientToServerMsg> =
                IpcSenderWithContext::new(stream);
            let _ = sender.send_client_msg(ClientToServerMsg::ConnStatus);
            let mut receiver: IpcReceiverWithContext<ServerToClientMsg> = sender.get_receiver();
            match receiver.recv_server_msg() {
                Some((ServerToClientMsg::Connected, _)) => true,
                None | Some((_, _)) => false,
            }
        },
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            drop(fs::remove_file(path));
            false
        },
        Err(_) => false,
    }
}

pub fn print_sessions(
    mut sessions: Vec<(String, Duration, bool)>,
    no_formatting: bool,
    short: bool,
    reverse: bool,
) {
    // (session_name, timestamp, is_dead)
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
        .for_each(|(session_name, timestamp, is_dead)| {
            if short {
                println!("{}", session_name);
                return;
            }
            if no_formatting {
                let suffix = if curr_session == *session_name {
                    format!("(current)")
                } else if *is_dead {
                    format!("(EXITED - attach to resurrect)")
                } else {
                    String::new()
                };
                let timestamp = format!("[Created {} ago]", format_duration(*timestamp));
                println!("{} {} {}", session_name, timestamp, suffix);
            } else {
                let formatted_session_name = format!("\u{1b}[32;1m{}\u{1b}[m", session_name);
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
                println!("{} {} {}", formatted_session_name, timestamp, suffix);
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

pub fn list_sessions(no_formatting: bool, short: bool, reverse: bool) {
    let exit_code = match get_sessions() {
        Ok(running_sessions) => {
            let resurrectable_sessions = get_resurrectable_sessions();
            let mut all_sessions: HashMap<String, (Duration, bool)> = resurrectable_sessions
                .iter()
                .map(|(name, timestamp)| (name.clone(), (timestamp.clone(), true)))
                .collect();
            for (session_name, duration) in running_sessions {
                all_sessions.insert(session_name.clone(), (duration, false));
            }
            if all_sessions.is_empty() {
                eprintln!("No active zellij sessions found.");
                1
            } else {
                print_sessions(
                    all_sessions
                        .iter()
                        .map(|(name, (timestamp, is_dead))| {
                            (name.clone(), timestamp.clone(), *is_dead)
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
}
