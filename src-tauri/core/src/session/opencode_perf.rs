//! Identical harness can be added to the PR baseline without changing its loader.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Instant;

#[test]
#[ignore = "explicit loopback performance run; reports measurements, not a CI timing gate"]
fn benchmark_opencode_conversation() {
    for (name, count, text_bytes) in [("small", 24usize, 128usize), ("large", 1200, 8192)] {
        let rows: Vec<_> = (0..count)
            .map(|i| {
                serde_json::json!({
                    "info":{"role":"assistant","time":{"created":i as i64 + 1}},
                    "parts":[{"type":"text","text":format!("{i:05} {}", "x".repeat(text_bytes))}]
                })
            })
            .collect();
        let pages: Vec<String> = rows
            .chunks(20)
            .rev()
            .map(|chunk| serde_json::to_string(chunk).unwrap())
            .collect();
        let page_count = pages.len();
        let wire_bytes: usize = pages.iter().map(String::len).sum();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let connection = Connection::parse(
            &format!("http://{}", listener.local_addr().unwrap()),
            "opencode".into(),
            None,
        )
        .unwrap();
        let iterations = 11;
        let server = std::thread::spawn(move || {
            for _ in 0..iterations {
                for (index, body) in pages.iter().enumerate() {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let cursor = if index + 1 < pages.len() {
                        format!("X-Next-Cursor: page{}\r\n", index + 1)
                    } else {
                        String::new()
                    };
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                        body.len(),
                        cursor,
                        body
                    )
                    .unwrap();
                }
            }
        });
        let cold_start = Instant::now();
        let client = client().unwrap();
        let mut times = Vec::new();
        let mut serialized_bytes = 0;
        for i in 0..iterations {
            let start = if i == 0 { cold_start } else { Instant::now() };
            let conversation = read_conversation(&connection, &client, "perf", false).unwrap();
            assert_eq!(conversation.messages.len(), count);
            assert!(conversation
                .messages
                .first()
                .unwrap()
                .content
                .starts_with("00000"));
            assert!(conversation
                .messages
                .last()
                .unwrap()
                .content
                .starts_with(&format!("{:05}", count - 1)));
            // Include serialization because GUI/WS transfer the full response.
            serialized_bytes = serde_json::to_vec(&conversation).unwrap().len();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        server.join().unwrap();
        let cold = times.remove(0);
        times.sort_by(f64::total_cmp);
        println!(
            "OPENCODE_PERF {}",
            serde_json::json!({
                "case":name,"messages":count,"pages":page_count,"wire_bytes":wire_bytes,
                "serialized_bytes":serialized_bytes,"cold_ms":cold,"repeat_median_ms":times[times.len()/2],
                "repeat_p95_ms":times[times.len()-1],"repeats":times.len(),"http_requests":page_count * iterations,
                "cache":"bypassed; every repeat rereads every HTTP page"
            })
        );
    }
}
