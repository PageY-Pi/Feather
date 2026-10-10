use feather_core::{
    agent::{Agent, AgentConfig, ProviderRef},
    compaction::CompactionSettings,
    message::{Api, ContentBlock, Message, Role},
    provider::faux::{FauxProvider, FauxScript},
    session::Session,
    tool::ToolRegistry,
};
use std::sync::Arc;
#[tokio::test]
async fn steering_precedes_next_turn_and_follow_up_waits_for_run_end() {
    let root = std::env::temp_dir().join(feather_core::provider::gen_id("queue"));
    std::fs::create_dir_all(&root).unwrap();
    let session = Session::new(&root.join(".feather"), &root).unwrap();
    let script = FauxScript::new()
        .with_tool("missing", serde_json::json!({}))
        .with_text("steered")
        .with_text("followed");
    let mut agent = Agent::new(
        AgentConfig {
            model: "test".into(),
            api: Api::Faux,
            system_prompt: "test".into(),
            max_tokens: 1024,
            temperature: None,
            context_window: 200_000,
            thinking_level: None,
            compaction: CompactionSettings::default(),
            compaction_model: None,
            tools: vec![].into(),
        },
        session,
        Arc::new(ToolRegistry::new()),
        ProviderRef::Faux(FauxProvider::new(script)),
        None,
    );
    let mut steering = Message::user_text("steer");
    steering.content.push(ContentBlock::Image {
        media_type: "image/png".into(),
        data: "AAAA".into(),
    });
    agent.input_queue().push(steering, true);
    agent.queue_follow_up("follow-up");
    let result = agent.prompt("initial").await.unwrap();
    assert_eq!(result.final_text, "followed");
    assert_eq!(
        result.turns, 3,
        "prompt result includes follow-up work, not only the final run"
    );
    let messages = agent.session().messages();
    let roles: Vec<_> = messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::Assistant,
            Role::ToolResult,
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant
        ]
    );
    assert_eq!(messages[3].text(), "steer");
    assert!(matches!(messages[3].content[1], ContentBlock::Image { .. }));
    assert_eq!(messages[5].text(), "follow-up");
    assert_eq!(
        agent.session().turn(),
        3,
        "tool turns also need durable snapshots"
    );
    assert_eq!(agent.input_queue().snapshot(), (vec![], vec![]));
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
