//! What happens to tool-result images when compaction has to fit the context.
//!
//! Images are the one block compaction cannot shorten: base64 has no "head and tail"
//! that means anything, and a truncated PNG is a corrupt image rather than a smaller
//! one. The tokenizer charges a flat 800 tokens for a block no matter how large its
//! payload is, which is right for a ~1 megapixel picture and badly wrong for a 4K
//! screenshot. So the questions that matter are not "is the image shortened" (it must
//! not be) but:
//!
//!   * does compaction leave the image intact rather than corrupting it,
//!   * does it still reclaim the *text* around the image, and
//!   * can it terminate when images alone exceed the budget?
//!
//! The last one is the real risk. Phase 2 elides the largest block until the estimate
//! fits, and an image measures 0 characters, so it is never chosen; if enough images
//! are present the loop runs its 64 iterations and returns without reaching the budget.
//! That is a bounded waste rather than an infinite loop, but it must not panic or spin,
//! and the caller must still see a usable context.
use feather_core::{
    compaction::{context_characters, shrink_messages_to_tokens},
    message::{ContentBlock, Message},
    tokenizer,
};

fn image(data: &str) -> ContentBlock {
    ContentBlock::Image {
        media_type: "image/png".into(),
        data: data.into(),
    }
}

/// A tool result shaped like one that read a screenshot: a short caption and an image
/// whose base64 is far larger than the caption.
fn tool_result_with_image(id: &str, caption: &str, width: usize) -> Message {
    let mut message = Message::tool_result(id, "read", caption, false);
    message.content.push(image(&"A".repeat(width)));
    message
}

#[test]
fn compaction_never_corrupts_or_drops_a_tool_result_image() {
    let mut messages = vec![Message::user_text("look at these screenshots")];
    for i in 0..6 {
        messages.push(tool_result_with_image(
            &format!("call_{i}"),
            &"x".repeat(4000),
            20_000,
        ));
    }
    let before: Vec<String> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Image { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(before.len(), 6);

    // A budget far below what these messages occupy, so every phase runs.
    shrink_messages_to_tokens(&mut messages, 500);

    let after: Vec<String> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Image { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect();
    for (i, data) in after.iter().enumerate() {
        assert_eq!(
            data.len(),
            20_000,
            "image {i} was truncated; a partial base64 payload decodes to a corrupt picture"
        );
        assert!(
            data.bytes().all(|b| b == b'A'),
            "image {i} was modified, not just resized"
        );
    }
    // Images may not be dropped either: if one is gone the model loses a picture it
    // already answered about, and the transcript would contradict itself.
    assert_eq!(
        after.len(),
        6,
        "compaction removed an image instead of the text around it"
    );
}

#[test]
fn compaction_still_reclaims_the_text_around_images() {
    let mut messages = vec![Message::user_text("start")];
    for i in 0..4 {
        messages.push(tool_result_with_image(
            &format!("call_{i}"),
            &"y".repeat(8000),
            1000,
        ));
    }
    let text_before: usize = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.len()),
            _ => None,
        })
        .sum();

    shrink_messages_to_tokens(&mut messages, 800);

    let text_after: usize = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.len()),
            _ => None,
        })
        .sum();
    assert!(
        text_after < text_before,
        "the bulky text around the images must still be elided ({text_before} -> {text_after})"
    );
}

/// A huge image must not become the block compaction picks, because an image cannot be
/// shortened and the elision loop would then give up on the whole context.
///
/// This is the case a naive change breaks. Phase 2 picks the block with the most
/// characters and elides it; if an image's base64 counted as characters, a big picture
/// would win that election every time, hit the `Image => return` arm, and abort the loop
/// with all the surrounding text untouched. The measured cost is the whole compaction
/// pass: nothing is reclaimed even though all of it could be.
#[test]
fn a_huge_image_does_not_stop_the_text_being_reclaimed() {
    let mut messages = vec![Message::user_text("start")];
    // A tool result whose text is small but whose image dwarfs everything else, next to
    // a plain text message that is the largest *shrinkable* block.
    messages.push(tool_result_with_image("call_1", "cap", 200_000));
    messages.push(Message::tool_result(
        "call_2",
        "grep",
        "z".repeat(50_000),
        false,
    ));

    let bulky_before = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } if text.starts_with('z') => Some(text.len()),
            _ => None,
        })
        .sum::<usize>();
    assert_eq!(bulky_before, 50_000);

    shrink_messages_to_tokens(&mut messages, 500);

    let bulky_after = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } if text.starts_with('z') => Some(text.len()),
            _ => None,
        })
        .sum::<usize>();
    assert!(
        bulky_after < bulky_before,
        "the 50k text block must still be elided even though a 200k image is present \
         ({bulky_before} -> {bulky_after}); counting image base64 as shrinkable text \
         makes the loop select the image and abort"
    );

    // And the image is still whole.
    let images: Vec<usize> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Image { data, .. } => Some(data.len()),
            _ => None,
        })
        .collect();
    assert_eq!(images, vec![200_000], "the image must survive untouched");
}

/// Images alone can exceed any budget. The loop that elides text must terminate and
/// leave the context intact rather than spin or panic.
#[test]
fn a_budget_below_the_image_cost_still_terminates() {
    let mut messages = vec![Message::user_text("start")];
    // 200 images at a flat 800 tokens each is ~160k tokens, far past the budget.
    for i in 0..200 {
        messages.push(tool_result_with_image(&format!("call_{i}"), "cap", 8));
    }
    let tokens_before = tokenizer::estimate_context_tokens(&messages);
    assert!(
        tokens_before > 100_000,
        "the fixture must exceed the budget by a wide margin, got {tokens_before}"
    );

    shrink_messages_to_tokens(&mut messages, 100);

    let image_count = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count();
    assert_eq!(
        image_count, 200,
        "no image may be dropped; the call must simply return when nothing can be elided"
    );
}

/// The character count deliberately excludes images, and that is what lets the caller
/// report savings without claiming to have shrunk a picture.
#[test]
fn images_do_not_count_toward_character_savings() {
    let plain = vec![Message::tool_result("c", "read", "abc", false)];
    let with_image = vec![tool_result_with_image("c", "abc", 100_000)];
    assert_eq!(
        context_characters(&plain),
        context_characters(&with_image),
        "a 100k base64 payload must add nothing to the character count, or savings would be fabricated"
    );
}

/// A tool result that carries both text and an image must keep both, in text-first
/// order, so the encoder has a text part to anchor the message.
#[test]
fn a_tool_result_keeps_its_text_and_image_together() {
    let message = tool_result_with_image("call_1", "the screenshot shows a chart", 32);
    assert_eq!(message.role, feather_core::message::Role::ToolResult);
    assert_eq!(message.text(), "the screenshot shows a chart");
    assert_eq!(
        message
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::Image { .. }))
            .count(),
        1
    );
    assert!(
        matches!(message.content[0], ContentBlock::Text { .. }),
        "text must come first so an encoder that ignores images still sends something"
    );
}

/// A picture inside the *kept* span must survive the in-place shrink pass.
///
/// `shrink_kept_span` rewrites an older tool result to its head+tail form by replacing
/// the whole block list with one text block. That drops any image in the same message,
/// silently: the text still looks shrunk, so nothing in the output says a picture went
/// away. A turn keeps its images only while it is within `keep_last_turns_unshrunk`
/// (2 by default), which is a much smaller guarantee than "inside the keep budget" —
/// a 20k-token keep span usually contains more than two turns.
#[test]
fn an_image_in_the_kept_span_survives_the_shrink_pass() {
    use feather_core::compaction::{shrink_kept_span, CompactionSettings};

    let settings = CompactionSettings {
        // Force the rewrite: a tiny budget makes every eligible result "too long".
        kept_tool_result_budget: 64,
        keep_last_turns_unshrunk: 1,
        ..CompactionSettings::default()
    };

    // Three turns. The oldest must be eligible for in-place shrinking; it carries both
    // a long text result and a picture.
    let mut messages = vec![Message::user_text("turn one")];
    messages.push(Message::tool_result("c1", "read", "a".repeat(5000), false));
    messages[1].content.push(ContentBlock::Image {
        media_type: "image/png".into(),
        data: "PICTURE".into(),
    });
    messages.push(Message::user_text("turn two"));
    messages.push(tool_result_with_image("c2", "b", 16));
    messages.push(Message::user_text("turn three"));
    messages.push(tool_result_with_image("c3", "c", 16));

    shrink_kept_span(&mut messages, &settings);

    let surviving: Vec<String> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Image { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !surviving.is_empty(),
        "the shrink pass removed every image. Text is still there, so nothing in the \
         output reports the loss: {:#?}",
        messages
    );
    assert!(
        surviving.iter().any(|d| d == "PICTURE"),
        "the oldest turn's image is the one eligible for shrinking; it must be kept \
         while its text is shortened, got {surviving:?}"
    );
    // The text half still has to be shortened, or the pass did nothing.
    let first_text = messages[1]
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    assert!(
        first_text < 5000,
        "the 5000-char text must still be shrunk, got {first_text}"
    );
}
