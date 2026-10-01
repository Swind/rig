#![allow(clippy::panic_in_result_fn)]
use super::*;

#[test]
fn endpoints_choose_api_and_normalize_the_base() -> Result<(), Error> {
    for (suffix, api) in [
        ("messages", Api::Messages),
        ("chat/completions", Api::Chat),
        ("responses", Api::Responses),
    ] {
        assert_eq!(
            endpoint(&format!("https://opencode.ai/zen/go/v1/{suffix}/"))?,
            (api, "https://opencode.ai/zen/go".into())
        );
    }
    for invalid in [
        "not a URL",
        "https://opencode.ai/zen/go/v1/models",
        "https://opencode.ai/zen/go/v1/messages?key=x",
        "ftp://opencode.ai/zen/go/v1/messages",
    ] {
        assert!(endpoint(invalid).is_err());
    }
    Ok(())
}

#[tokio::test]
async fn all_go_protocols_stream_with_correct_auth_and_conversation_headers()
-> Result<(), Box<dyn std::error::Error>> {
    use futures::TryStreamExt;
    use rig_agent::agent::MultiTurnStreamItem;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    let chat = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"id":"chat1","object":"chat.completion.chunk","created":0,"model":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":"reply"},"finish_reason":null}]}),
        json!({"id":"chat1","object":"chat.completion.chunk","created":0,"model":"fixture","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
    );
    let response = json!({"id":"resp1","object":"response","created_at":1,"status":"completed","error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"model":"fixture","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"output":[{"type":"message","id":"msg1","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"reply"}]}],"tools":[]});
    let responses = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"type":"response.completed","sequence_number":0,"response":response})
    );
    let messages = [
        ("message_start",json!({"type":"message_start","message":{"id":"msg1","type":"message","role":"assistant","model":"fixture","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":0}}})),
        ("content_block_start",json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
        ("content_block_delta",json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"reply"}})),
        ("content_block_stop",json!({"type":"content_block_stop","index":0})),
        ("message_delta",json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}})),
        ("message_stop",json!({"type":"message_stop"})),
    ].into_iter().map(|(event,value)|format!("event: {event}\ndata: {value}\n\n")).collect::<String>();

    for (route, sse, messages_api) in [
        ("chat/completions", chat, false),
        ("responses", responses, false),
        ("messages", messages, true),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/v1/{route}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..3 {
                let (socket, _) = listener.accept().await?;
                let mut reader = tokio::io::BufReader::new(socket);
                let mut start = String::new();
                reader.read_line(&mut start).await?;
                let mut headers = std::collections::HashMap::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await?;
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                    }
                }
                let length = headers
                    .get("content-length")
                    .ok_or_else(|| std::io::Error::other("missing length"))?
                    .parse::<usize>()
                    .map_err(std::io::Error::other)?;
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await?;
                requests.push((start, headers, body));
                reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",sse.len()).as_bytes()).await?;
            }
            Ok::<_, std::io::Error>(requests)
        });
        let agent = AgentBuilder::new(go_model("offline-go-key".into(), "fixture".into(), &url)?)
            .memory(InMemoryConversationMemory::new())
            .max_tokens(4096)
            .add_hook(ModelLimits {
                context: Some(100_000),
                output: Some(2048),
            })
            .build();
        for session in ["conversation-one", "conversation-one", "conversation-two"] {
            let items = SESSION
                .scope(session.into(), async {
                    agent
                        .prompt("hello")
                        .conversation(session)
                        .stream()
                        .try_collect::<Vec<_>>()
                        .await
                })
                .await?;
            assert!(items.iter().any(|item|matches!(item,MultiTurnStreamItem::FinalResponse(reply) if reply.output()=="reply")));
        }
        let requests = tokio::time::timeout(Duration::from_secs(5), server).await???;
        for ((start, headers, body), session) in
            requests
                .iter()
                .zip(["conversation-one", "conversation-one", "conversation-two"])
        {
            assert!(start.starts_with(&format!("POST /v1/{route} ")));
            assert_eq!(
                headers.get("x-opencode-session").map(String::as_str),
                Some(session)
            );
            assert!(
                headers
                    .get("user-agent")
                    .is_some_and(|agent| agent.starts_with("rig-messaging-slack/"))
            );
            let auth = if messages_api {
                "x-api-key"
            } else {
                "authorization"
            };
            let expected = if messages_api {
                "offline-go-key"
            } else {
                "Bearer offline-go-key"
            };
            assert_eq!(headers.get(auth).map(String::as_str), Some(expected));
            let body: serde_json::Value = serde_json::from_slice(body)?;
            assert_eq!(
                body.get("model").and_then(serde_json::Value::as_str),
                Some("fixture")
            );
            let tokens = body
                .get("max_tokens")
                .or_else(|| body.get("max_output_tokens"));
            assert_eq!(tokens.and_then(serde_json::Value::as_u64), Some(2048));
        }
    }
    Ok(())
}

#[test]
fn context_budget_handles_missing_and_exhausted_limits() {
    let limits = ModelLimits {
        context: Some(10000),
        output: Some(8192),
    };
    assert_eq!(limits.budget(20000, 0), Some(5904));
    assert_eq!(limits.budget(4096, 4000), Some(1904));
    assert_eq!(limits.budget(4096, 5904), None);
    assert_eq!(limits.budget(4096, usize::MAX), None);
    assert_eq!(ModelLimits::default().budget(4096, usize::MAX), Some(4096));
}

#[tokio::test]
#[ignore = "requires OpenCode Go credentials and consumes provider quota"]
async fn live_go_streams_two_turns_with_conversation_history()
-> Result<(), Box<dyn std::error::Error>> {
    use futures::TryStreamExt;
    use rig_agent::agent::MultiTurnStreamItem;
    dotenvy::dotenv()?;
    let (agent, go) = agent_from_env().await?;
    assert!(go, "live Go validation requires OPENCODE_GO_API_KEY");
    let session = format!("rig-messaging-provider-smoke-{}", std::process::id());
    for prompt in [
        "In Rust, what does Iterator::map do? Explain in at most 30 words.",
        "Give one Rust code example for that method, in at most 30 words.",
    ] {
        let items = SESSION
            .scope(session.clone(), async {
                agent
                    .prompt(prompt)
                    .conversation(session.as_str())
                    .stream()
                    .try_collect::<Vec<_>>()
                    .await
            })
            .await?;
        assert!(items.iter().any(|item| matches!(item,
            MultiTurnStreamItem::FinalResponse(response) if !response.output().trim().is_empty()
        )));
    }
    eprintln!("Live Go validation passed: two streamed Rust assistance turns");
    Ok(())
}

#[tokio::test]
async fn go_requests_fail_if_conversation_context_is_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    assert!(
        SessionHeader
            .before_request_headers(
                &Method::POST,
                &"https://example.invalid".parse()?,
                &mut headers
            )
            .await
            .is_err()
    );
    Ok(())
}
