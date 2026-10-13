//! Offline HTTP fixtures. All credentials in these tests are synthetic.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub(crate) struct MockServer {
    pub url: String,
    pub requests: Receiver<String>,
    thread: std::thread::JoinHandle<()>,
}

impl MockServer {
    pub fn finish(self) {
        self.thread.join().unwrap();
    }
}

pub(crate) fn serve(responses: Vec<(u16, &'static str, String)>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, requests) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        for (status, content_type, body) in responses {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("mock HTTP accept: {e}"),
                }
            };
            // Accepted sockets may inherit nonblocking mode on Windows.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            let end = loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                assert!(bytes.len() < 1_000_000);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let header = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length = header
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .and_then(|s| s.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            while bytes.len() < end + length {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            tx.send(String::from_utf8(bytes).unwrap()).unwrap();
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    MockServer {
        url,
        requests,
        thread,
    }
}
