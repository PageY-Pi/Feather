//! Real-socket regression: the provider traffic limiter bounds aggregate
//! throughput for streamed responses, and clearing it restores full speed.
//! Entirely localhost and synthetic; no credentials or OS config are read.
use feather_core::{
    message::Message,
    provider::{
        openai::OpenAiProvider,
        transport::{self, Route},
        CacheHints, ChatRequest, StreamEvent,
    },
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};

/// Payload size large enough that shaping is measurable but the test stays
/// fast: 256 KB at 512 KB/s is about half a second.
const BODY_BYTES: usize = 256 * 1024;
const LIMIT: u64 = 512 * 1024;

/// Serve `connections` sequential SSE responses, each roughly `BODY_BYTES`.
/// The server writes as fast as the socket allows, so any delay the client
/// observes comes from the limiter rather than from the peer.
fn streaming_server(connections: usize) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = std::thread::spawn(move || {
        for _ in 0..connections {
            serve_one(&listener);
        }
    });
    (url, task)
}

fn serve_one(listener: &TcpListener) {
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    let (end, len) = loop {
        let n = socket.read(&mut buffer).unwrap();
        assert!(n > 0, "client closed before sending a request");
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let len = headers
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            break (end + 4, len);
        }
    };
    while bytes.len() < end + len {
        let n = socket.read(&mut buffer).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
    }

    // A stream of small SSE events totals BODY_BYTES of content.
    let chunk = "x".repeat(4096);
    let mut payload = String::new();
    while payload.len() < BODY_BYTES {
        payload.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{chunk}\"}}}}]}}\n\n"
        ));
    }
    payload.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    payload.push_str("data: [DONE]\n\n");
    write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
        .unwrap();
    // Drain until the client has consumed the whole body, so the timing
    // observed by the test reflects the wire, not a truncated write.
    let mut sink = [0_u8; 4096];
    let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
    while let Ok(n) = socket.read(&mut sink) {
        if n == 0 {
            break;
        }
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message::user_text("hello")].into(),
        tools: Vec::new().into(),
        model: "synthetic".into(),
        max_tokens: 64,
        temperature: None,
        cache: CacheHints::default(),
        thinking: None,
        session_id: "rate-limit".into(),
    }
}

/// Drain a response stream, returning (elapsed, bytes of content received).
async fn drain(provider: &OpenAiProvider) -> (Duration, usize) {
    let mut stream = provider.stream(&request()).await.expect("stream starts");
    let started = Instant::now();
    let mut content = 0;
    while let Some(event) = stream.next().await {
        let event = event.expect("stream stays healthy");
        if let StreamEvent::TextDelta(text) = event {
            content += text.len();
        }
    }
    (started.elapsed(), content)
}

#[tokio::test]
async fn rate_limit_bounds_response_throughput_and_clears() {
    transport::set_direct();
    let (origin, server_thread) = streaming_server(3);
    let provider = OpenAiProvider::new("SYNTHETIC", Some(format!("{origin}/v1")));

    // Unlimited first: draining 256 KB over loopback must be quick.
    transport::set_rate_limit(0);
    let (unlimited, unlimited_bytes) = drain(&provider).await;
    assert!(
        unlimited_bytes >= BODY_BYTES,
        "expected the full synthetic body, got {unlimited_bytes} bytes"
    );
    assert!(
        unlimited < Duration::from_millis(500),
        "unlimited loopback transfer should be fast, took {unlimited:?}"
    );

    // Shaped: about 256 KB / 512 KB/s = 500 ms. Allow generous slack for CI.
    transport::set_rate_limit(LIMIT);
    let (shaped, shaped_bytes) = drain(&provider).await;
    transport::set_rate_limit(0);
    assert_eq!(
        shaped_bytes, unlimited_bytes,
        "shaping must not truncate the body"
    );
    assert!(
        shaped >= Duration::from_millis(300),
        "0.5 s of traffic must not complete instantly, took {shaped:?}"
    );
    assert!(
        shaped < Duration::from_secs(5),
        "shaping must not stall the stream, took {shaped:?}"
    );
    assert!(
        shaped > unlimited,
        "shaped run ({shaped:?}) should be slower than the unlimited run ({unlimited:?})"
    );

    // Clearing the limit restores full speed immediately.
    let (restored, _) = drain(&provider).await;
    assert!(
        restored < Duration::from_millis(500),
        "clearing the limit must restore full speed, took {restored:?}"
    );

    server_thread.join().unwrap();
    assert_eq!(Route::Direct, transport::current_route());
}
