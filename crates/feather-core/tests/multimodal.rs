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
