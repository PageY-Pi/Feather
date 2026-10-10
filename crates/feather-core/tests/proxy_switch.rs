//! Real socket regression: existing providers change routes on the next request.
//! Entirely localhost and synthetic; does not read real credentials or OS config.
use feather_core::{provider::{transport::{self,Route},openai::OpenAiProvider,ChatRequest,CacheHints,StreamEvent},message::Message};
use std::{io::{Read,Write},net::TcpListener,sync::mpsc,time::Duration};
fn server(answer:&'static str)->(String,mpsc::Receiver<String>,std::thread::JoinHandle<()>) {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();let url=format!("http://{}",listener.local_addr().unwrap());let(tx,rx)=mpsc::channel();
    let task=std::thread::spawn(move|| {
        let(mut socket,_)=listener.accept().unwrap();socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();let mut bytes=Vec::new();let mut buffer=[0;4096];
        let(end,len)=loop {let n=socket.read(&mut buffer).unwrap();assert!(n>0);bytes.extend_from_slice(&buffer[..n]);
            if let Some(end)=bytes.windows(4).position(|w|w==b"\r\n\r\n") {
                let headers=String::from_utf8_lossy(&bytes[..end]).to_lowercase();let len=headers.lines().find_map(|l|l.strip_prefix("content-length:").and_then(|v|v.trim().parse::<usize>().ok())).unwrap_or(0);break(end+4,len);
            }
        };
        while bytes.len()<end+len{let n=socket.read(&mut buffer).unwrap();assert!(n>0);bytes.extend_from_slice(&buffer[..n]);}
        tx.send(String::from_utf8(bytes).unwrap()).unwrap();
        let payload=format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{answer}\"}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n");
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).unwrap();
    });(url,rx,task)
}
#[tokio::test]
async fn same_provider_routes_through_selected_proxy_then_direct_without_restart() {
    let(origin,origin_requests,origin_thread)=server("direct");let(proxy,proxy_requests,proxy_thread)=server("proxy");
    transport::set_direct();let provider=OpenAiProvider::new("SYNTHETIC",Some(format!("{origin}/v1")));
    let request=ChatRequest{system:"test".into(),messages:vec![Message::user_text("hello")].into(),tools:Vec::new().into(),model:"synthetic".into(),max_tokens:32,temperature:None,cache:CacheHints::default(),thinking:None,session_id:"proxy-test".into()};
    transport::set_route(Route::proxy(proxy).unwrap());
    let mut stream=provider.stream(&request).await.unwrap();let mut text=String::new();while let Some(e)=stream.next().await{if let StreamEvent::TextDelta(t)=e.unwrap(){text.push_str(&t)}}assert_eq!(text,"proxy");
    let wire=proxy_requests.recv_timeout(Duration::from_secs(2)).unwrap();assert!(wire.starts_with(&format!("POST {origin}/v1/chat/completions")));proxy_thread.join().unwrap();
    transport::set_direct();
    let mut stream=provider.stream(&request).await.unwrap();let mut text=String::new();while let Some(e)=stream.next().await{if let StreamEvent::TextDelta(t)=e.unwrap(){text.push_str(&t)}}assert_eq!(text,"direct");
    assert!(origin_requests.recv_timeout(Duration::from_secs(2)).unwrap().starts_with("POST /v1/chat/completions"));origin_thread.join().unwrap();
}
