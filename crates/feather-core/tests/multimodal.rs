use feather_core::{
    message::{ContentBlock, Message},
    provider::{
        anthropic::AnthropicProvider, openai::OpenAiProvider, CacheHints, ChatRequest,
        ThinkingLevel,
    },
};
fn request() -> ChatRequest {
    let mut message = Message::user_text("look");
    message.content.push(ContentBlock::Image {
        media_type: "image/png".into(),
        data: "AAAA".into(),
    });
    ChatRequest {
        system: "test".into(),
        messages: vec![message].into(),
        tools: vec![].into(),
        model: "test".into(),
        max_tokens: 8192,
        temperature: Some(0.5),
        cache: CacheHints::default(),
        thinking: None,
        session_id: String::new(),
    }
}
#[test]
fn images_are_provider_native_not_base64_prompt_text() {
    let req = request();
    let a = AnthropicProvider::build_body(&req);
    assert_eq!(
        a["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(a["messages"][0]["content"][1]["source"]["data"], "AAAA");
    let o = OpenAiProvider::build_body(&req);
    assert_eq!(
        o["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
    assert_eq!(req.messages[0].text(), "look");
    let encoded = serde_json::to_string(&req.messages[0]).unwrap();
    assert_eq!(
        serde_json::from_str::<Message>(&encoded).unwrap(),
        req.messages[0]
    );
}
#[test]
fn thinking_matches_pi_default_budgets_and_preserves_openai_effort() {
    let mut req = request();
    req.thinking = Some(ThinkingLevel::High);
    let a = AnthropicProvider::build_body(&req);
    assert_eq!(a["thinking"]["budget_tokens"], 7168);
    assert!(
        a.get("temperature").is_none(),
        "temperature is incompatible with extended thinking"
    );
    for level in [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ] {
        req.thinking = Some(level);
        assert_eq!(
            OpenAiProvider::build_body(&req)["reasoning_effort"],
            level.as_str()
        );
    }
    assert_eq!(ThinkingLevel::Xhigh.budget_tokens(), 16384);
    assert_eq!(ThinkingLevel::Max.budget_tokens(), 16384);
    req.thinking = Some(ThinkingLevel::Off);
    assert_eq!(
        AnthropicProvider::build_body(&req)["thinking"]["type"],
        "disabled"
    );
    assert!(OpenAiProvider::build_body(&req)
        .get("reasoning_effort")
        .is_none());
}

#[test]
fn multimodal_blocks_keep_order_and_image_only_has_no_empty_text_block() {
    let mut req = request();
    std::sync::Arc::make_mut(&mut req.messages)[0]
        .content
        .push(ContentBlock::Text {
            text: "after".into(),
        });
    let a = AnthropicProvider::build_body(&req);
    let o = OpenAiProvider::build_body(&req);
    assert_eq!(a["messages"][0]["content"][2]["text"], "after");
    assert_eq!(o["messages"][1]["content"][2]["text"], "after");
    std::sync::Arc::make_mut(&mut req.messages)[0]
        .content
        .remove(0);
    std::sync::Arc::make_mut(&mut req.messages)[0].content.pop();
    assert_eq!(
        AnthropicProvider::build_body(&req)["messages"][0]["content"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        OpenAiProvider::build_body(&req)["messages"][1]["content"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// A tool result image cannot ride in `role:"tool"`, because that field is a string on
/// every OpenAI-compatible gateway. It is lifted into a following user message.
///
/// Measured before this was written: the same shape sent to `opencode-go` and
/// `volcengine-plan` both came back naming the colour that was drawn, so the split is
/// accepted where it matters and not only in theory.
#[test]
fn a_tool_result_image_is_lifted_into_a_following_user_message() {
    let call = feather_core::message::ToolCall {
        id: "call_1".into(),
        name: "read".into(),
        arguments: serde_json::json!({"path": "a.png"}),
    };
    let mut assistant = Message {
        role: feather_core::message::Role::Assistant,
        content: vec![ContentBlock::ToolCall(call)],
        tool_call_id: None,
        tool_name: None,
        is_error: false,
        model: None,
        api: None,
        usage: None,
        stop_reason: None,
        timestamp: 0,
        shrink: None,
    };
    assistant.content.push(ContentBlock::Text {
        text: "reading".into(),
    });

    let mut tool = Message::tool_result("call_1", "read", "", false);
    tool.content.push(ContentBlock::Image {
        media_type: "image/png".into(),
        data: "BBBB".into(),
    });

    let req = ChatRequest {
        system: "s".into(),
        messages: vec![Message::user_text("show me"), assistant, tool].into(),
        tools: vec![].into(),
        model: "m".into(),
        max_tokens: 64,
        temperature: None,
        cache: CacheHints::default(),
        thinking: None,
        session_id: String::new(),
    };
    let body = OpenAiProvider::build_body(&req);
    let messages = body["messages"].as_array().unwrap();

    // The tool message itself must stay a string: an array here is a 400.
    let tool_msg = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the tool result must still be present");
    assert!(
        tool_msg["content"].is_string(),
        "role:\"tool\" content must stay a string, got {}",
        tool_msg["content"]
    );
    assert_eq!(
        tool_msg["content"], "(see attached image)",
        "an image-only result needs placeholder text, or the model sees an empty result"
    );

    // The image rides in the message right after it.
    let carrier = messages
        .iter()
        .position(|m| m["role"] == "tool")
        .and_then(|i| messages.get(i + 1))
        .expect("a carrier message must follow the tool result");
    assert_eq!(carrier["role"], "user");
    assert_eq!(
        carrier["content"][1]["image_url"]["url"],
        "data:image/png;base64,BBBB"
    );
    assert_eq!(carrier["content"][0]["type"], "text");
}

/// A tool result with no images must encode exactly as it did before the feature.
#[test]
fn a_tool_result_without_images_does_not_gain_a_carrier_message() {
    let req = ChatRequest {
        system: "s".into(),
        messages: vec![
            Message::user_text("hi"),
            Message::tool_result("call_1", "read", "plain text", false),
        ]
        .into(),
        tools: vec![].into(),
        model: "m".into(),
        max_tokens: 64,
        temperature: None,
        cache: CacheHints::default(),
        thinking: None,
        session_id: String::new(),
    };
    let body = OpenAiProvider::build_body(&req);
    let messages = body["messages"].as_array().unwrap();
    // system + user + tool: exactly what this request is, with nothing appended.
    assert_eq!(
        messages.len(),
        3,
        "no carrier message should appear when the result has no image: {messages:#?}"
    );
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["content"], "plain text");
}
