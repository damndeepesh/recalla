//! `recalla ui` end to end: spawn the binary, read the URL, load the page.

#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::Duration;

fn get(port: &str, target: &str, cookie: Option<&str>) -> String {
    let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let cookie = cookie.map_or_else(String::new, |c| format!("Cookie: {c}\r\n"));
    write!(
        s,
        "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{cookie}\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[test]
fn ui_prints_one_time_code_url_and_serves_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(["ui", "--no-open", "--port", "0", "--data-dir"])
        .arg(dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    let second = lines.next().unwrap().unwrap();
    let third = lines.next().unwrap().unwrap();
    let url = first
        .strip_prefix("Recalla is running at http://127.0.0.1:")
        .unwrap();
    let (port, code) = url.split_once("/?c=").unwrap();
    assert_eq!(
        second,
        "This link works once, for 60 seconds. Run recalla ui again for a new one."
    );
    assert_eq!(third, "Press Ctrl-C to stop");
    assert_eq!(code.len(), 64);
    assert!(code.bytes().all(|b| b.is_ascii_hexdigit()));

    let login = get(port, &format!("/?c={code}"), None);
    let cookie = login
        .lines()
        .find_map(|l| {
            l.strip_prefix("set-cookie: ")
                .or_else(|| l.strip_prefix("Set-Cookie: "))
        })
        .and_then(|c| c.split(';').next())
        .unwrap()
        .to_owned();
    let token = cookie.strip_prefix(&format!("recalla_{port}=")).unwrap();
    assert!(login.starts_with("HTTP/1.1 302"), "{login}");
    assert_ne!(token, code);
    assert!(!format!("{first}{second}{third}").contains(token));
    assert!(get(port, &format!("/?c={code}"), None).starts_with("HTTP/1.1 401"));
    let ping = get(port, "/api/ping", Some(&cookie));
    child.kill().unwrap();
    let _ = child.wait();
    assert!(ping.starts_with("HTTP/1.1 200"), "{ping}");
}

#[test]
fn ui_bind_failure_exits_2() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(["ui", "--no-open", "--port", &port, "--data-dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot bind"));
}

#[cfg(unix)] // the fake opener is a shell script
#[test]
fn ui_opens_a_private_login_page_not_the_code_url() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let argv = bin.path().join("argv.txt");
    // Fake opener: records argv and a copy of the page it was pointed at.
    for name in ["open", "xdg-open"] {
        let p = bin.path().join(name);
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}'\ncat \"$1\" > '{0}.page'\n",
                argv.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:/usr/bin:/bin", bin.path().display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_recalla"))
        .args(["ui", "--port", "0", "--data-dir"])
        .arg(dir.path())
        .env("PATH", path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    let (port, code) = first
        .strip_prefix("Recalla is running at http://127.0.0.1:")
        .and_then(|u| u.split_once("/?c="))
        .unwrap();
    for _ in 0..50 {
        if argv.exists() {
            std::thread::sleep(Duration::from_millis(100));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let opened = std::fs::read_to_string(&argv).unwrap();
    let page_path = std::path::PathBuf::from(opened.trim());
    assert!(page_path.starts_with(dir.path().join("run")), "{opened}");
    assert!(!opened.contains(code));
    let page = std::fs::read_to_string(format!("{}.page", argv.display())).unwrap();
    assert!(page.contains(&format!("http://127.0.0.1:{port}/?c={code}")));
    assert!(page_path.exists());
    assert!(get(port, &format!("/?c={code}"), None).starts_with("HTTP/1.1 302"));
    let gone = (0..50).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        !page_path.exists()
    });
    child.kill().unwrap();
    let _ = child.wait();
    assert!(gone, "login page not deleted after redemption");
}
