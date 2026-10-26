//! Regression: compaction must leave working headroom, not land on its trigger.
//!
//! The reported symptom: after compaction the rewrite sat a few tokens under
//! the trigger, so the very next turn compacted again -- a summarization request
//! every turn for no gain.
use feather_core::compaction;
use feather_core::message::{Api, ContentBlock, Message, StopReason, ToolCall, Usage};
use feather_core::tokenizer;

fn answer(text: &str) -> Message {
    Message::assistant(
        vec![ContentBlock::Text { text: text.into() }],
        "faux",
        Api::Faux,
        Usage::default(),
        StopReason::Stop,
    )
}

fn tool_call(q: usize, args: usize) -> Message {
    Message::assistant(
        vec![ContentBlock::ToolCall(ToolCall {
            id: format!("c{q}"),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "x".repeat(args)}),
        })],
        "faux",
        Api::Faux,
        Usage::default(),
        StopReason::ToolUse,
    )
}

/// One plain answer early in the session, then prompt after prompt of long tool
/// loops. The newest plain answer therefore stays far behind the tail, which is
/// the state compaction sees when it fires at the top of a turn.
fn long_agentic_session() -> Vec<Message> {
    let mut msgs = vec![Message::user_text("old question"), answer("EARLY ANSWER")];
    for round in 0..8u32 {
        msgs.push(Message::user_text(format!("job {round}")));
        for q in 0..120u32 {
            msgs.push(tool_call((round * 120 + q) as usize, 200));
            msgs.push(Message::tool_result(
                format!("c{round}-{q}"),
                "bash",
                "x".repeat(2_000),
                false,
            ));
        }
    }
    msgs
}

#[test]
fn budget_cut_does_not_keep_the_whole_history() {
    let msgs = long_agentic_session();
    let keep = 20_000u64;
    let cut = compaction::plan_cut_budget(&msgs, keep);
    let kept: u64 = msgs[cut..]
        .iter()
        .map(tokenizer::estimate_message_tokens_without_thinking)
        .sum();
    assert!(
        kept < keep * 2,
        "budget cut kept {kept} tokens (cut={cut} of {}): a stale early answer \
         must not pin the kept span to the whole history",
        msgs.len()
    );
}

#[test]
fn rewrite_targets_below_the_trigger() {
    // The rewrite budget must sit a keep-budget below the trigger, otherwise the
    // rewrite lands on the line and one tool result re-fires it.
    let settings = feather_core::compaction::CompactionSettings {
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
        ..Default::default()
    };
    let trigger = 272_000u64 - settings.reserve_tokens;
    let room = compaction::compaction_room(trigger, 0, &settings);
    assert_eq!(
        room,
        trigger - settings.keep_recent_tokens,
        "the rewrite budget must reserve the keep budget below the trigger"
    );
    // Static request tokens come off first and never underflow.
    let room = compaction::compaction_room(trigger, 5_000, &settings);
    assert_eq!(room, trigger - 5_000 - settings.keep_recent_tokens);
    // Available room is 100 and the keep budget is halved to 50, so the rewrite
    // still targets 50 tokens rather than collapsing to zero.
    assert_eq!(compaction::compaction_room(1_000, 900, &settings), 50);

    // A keep budget at or above the available room is a misconfiguration; the
    // reservation must stay bounded so compaction still leaves a context.
    let tiny = feather_core::compaction::CompactionSettings {
        reserve_tokens: 100,
        keep_recent_tokens: 100_000,
        ..Default::default()
    };
    assert_eq!(compaction::compaction_room(1_000, 0, &tiny), 500);
}
