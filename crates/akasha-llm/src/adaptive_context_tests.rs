//! Real HTTP transport with deterministic Ollama metadata and model responses.
use crate::provider::LLMProvider;
use crate::{CompletionRequest, ConversationMessage, OllamaProvider};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

#[tokio::test]
async fn discovered_allocation_and_chunked_sources_work_in_both_transports() {
    for context in [8192, 32768] {
        for streaming in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let started = Instant::now();
                let mut summaries = 0;
                loop {
                    assert!(
                        started.elapsed() < Duration::from_secs(12),
                        "missing final model request"
                    );
                    let (mut socket, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("{error}"),
                    };
                    socket
                        .set_nonblocking(false)
                        .unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let end = loop {
                        let n = socket.read(&mut buffer).unwrap();
                        assert!(n > 0);
                        bytes.extend_from_slice(&buffer[..n]);
                        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                            break index + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    while bytes.len() < end + length {
                        let n = socket.read(&mut buffer).unwrap();
                        assert!(n > 0);
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    let mut final_request = false;
                    let reply = if headers.starts_with("post /api/show ") {
                        json!({"template":"{{ range .Messages }}{{ .Content }}{{ end }}", "model_info":{"general.architecture":"test","test.context_length":131072}})
                    } else if headers.starts_with("get /api/ps ") {
                        json!({"models":[{"name":"test:latest","context_length":context}]})
                    } else {
                        assert!(headers.starts_with("post /api/chat "), "{headers}");
                        let request: Value =
                            serde_json::from_slice(&bytes[end..end + length]).unwrap();
                        assert_eq!(request["model"], "test");
                        assert_eq!(request["options"]["num_ctx"], context);
                        let messages = request["messages"].as_array().unwrap();
                        assert!(messages
                            .iter()
                            .all(|message| message.get("context_data").is_none()));
                        let source_chunk = messages.last().unwrap()["content"]
                            .as_str()
                            .unwrap()
                            .contains("[Source data;");
                        let text = if source_chunk {
                            summaries += 1;
                            assert!(request.get("tools").is_none());
                            assert_eq!(request["stream"], false);
                            "Source facts: https://example.test/source, exact value 42."
                        } else {
                            final_request = true;
                            assert_eq!(messages.first().unwrap()["content"], "Exact system rules");
                            assert_eq!(
                                messages.last().unwrap()["content"],
                                "Exact current request"
                            );
                            "Final answer"
                        };
                        json!({"model":"test","message":{"content":text},"done":true,"done_reason":"stop","prompt_eval_count":10,"eval_count":2})
                    };
                    let body = format!("{reply}\n");
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    if final_request {
                        return summaries;
                    }
                }
            });
            let provider = OllamaProvider::new(Some(format!("http://{address}")));
            let mut request: CompletionRequest = serde_json::from_value(json!({"prompt":"Exact current request","system_prompt":"Exact system rules","max_tokens":1024,"conversation":[]})).unwrap();
            request
                .conversation
                .as_mut()
                .unwrap()
                .push(ConversationMessage::context_data(
                    "Document data with value 42. ".repeat(1500),
                ));
            let (tx, rx) = std::sync::mpsc::channel();
            let result = if streaming {
                provider
                    .complete_stream_async(&request, "test", Duration::from_secs(10), &tx)
                    .await
            } else {
                provider
                    .complete_async(&request, "test", Duration::from_secs(10))
                    .await
            }
            .unwrap();
            assert_eq!(result.text, "Final answer");
            let chunks: String = rx.try_iter().collect();
            if streaming {
                assert_eq!(chunks, "Final answer");
            } else {
                assert!(chunks.is_empty());
            }
            let summaries = server.join().unwrap();
            if context == 8192 {
                assert!(summaries > 1);
            } else {
                assert_eq!(summaries, 0);
            }
        }
    }
}
