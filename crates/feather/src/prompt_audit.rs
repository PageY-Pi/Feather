//! Static checks on the system prompt.
//!
//! A rule that cannot fail cannot be improved: nothing distinguishes better
//! wording from worse. These are the parts of that problem which are mechanically
//! decidable, so a prompt edit gets a verdict in milliseconds instead of needing a
//! benchmark run to say anything at all.
//!
//! Three gates, each of which can actually fail:
//!
//! - `OverBudget` — the prompt grew past its token ceiling.
//! - `DeadToolReference` — the prompt names a tool that no longer exists. This is
//!   the drift that hurts most: a renamed tool leaves a sentence that reads fine
//!   and instructs the model to call something it cannot call.
//! - `DuplicateRule` / `RuleInsideRule` — the same instruction stated twice. Two
//!   rules that say the same thing spend attention twice and can be obeyed
//!   inconsistently.
//!
//! **What is deliberately not checked.** Falsifiability — whether a rule could
//! ever be observed to be broken — is not mechanised here. The versions of it
//! that are mechanical ("must contain an identifier", "must not use a manner
//! adverb") either fire on sound rules or degrade into a word blacklist, and a
//! prompt bent to satisfy a linter is worse than one a human reviews. The witness
//! that makes a rule falsifiable is a scenario where it fails, which is what the
//! behavioural layer will supply; when it exists, the check to add is "every rule
//! has at least one scenario", not a heuristic over the wording. Until then this
//! module stays silent rather than guessing.
//!
//! Tool names are never listed here. They are read from the live registry, so this
//! file cannot drift from the thing it audits.

use std::collections::BTreeSet;

/// The base instructions must stay small: they are billed on every request.
///
/// This is a ceiling, not a target. It sits above the current size on purpose so
/// ordinary wording changes do not trip it, and raising it is a decision that
/// needs an argument, not a reflex.
pub const TOKEN_BUDGET: u64 = 260;

/// One instruction the prompt gives the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// 1-based line number in the prompt, for pointing a human at the text.
    pub line: usize,
    pub text: String,
}

/// A way the prompt is wrong. Every variant is reachable by a plausible edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Defect {
    /// The prompt has no instructions at all — every other check is then vacuous.
    NoRules,
    OverBudget {
        tokens: u64,
        budget: u64,
    },
    /// A backticked name looks like a tool but the registry does not have it.
    DeadToolReference {
        line: usize,
        name: String,
    },
    /// Two rules reduce to the same words.
    DuplicateRule {
        first: usize,
        second: usize,
    },
    /// One rule's text contains another's, so the inner one is redundant.
    RuleInsideRule {
        outer: usize,
        inner: usize,
    },
}

impl Defect {
    /// A sentence a human can act on. Tests assert on variants; this is for logs.
    pub fn explain(&self) -> String {
        match self {
            Defect::NoRules => "the prompt contains no instructions".into(),
            Defect::OverBudget { tokens, budget } => {
                format!("prompt is ~{tokens} tokens, over the {budget} budget")
            }
            Defect::DeadToolReference { line, name } => {
                format!("line {line} names `{name}`, which is not a registered tool")
            }
            Defect::DuplicateRule { first, second } => {
                format!("lines {first} and {second} state the same rule")
            }
            Defect::RuleInsideRule { outer, inner } => {
                format!("line {inner} is contained in line {outer}")
            }
        }
    }
}

/// The instruction lines of a prompt.
///
/// The role sentence and bare headers (`Working principles:`) are framing, not
/// rules. Everything else counts — a trailing sentence after the bullet list is
/// an instruction whether or not it has a dash in front of it, and treating only
/// bullets as rules would let a whole paragraph escape the audit.
pub fn rules(prompt: &str) -> Vec<Rule> {
    let mut out = Vec::new();
    for (index, raw) in prompt.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if index == 0 && line.starts_with("You are ") {
            continue;
        }
        // A header is a line that ends in a colon and does not itself instruct.
        if line.ends_with(':') && !line[..line.len() - 1].contains(' ') {
            continue;
        }
        if line.ends_with(':') && line.split_whitespace().count() <= 3 {
            continue;
        }
        let text = line.strip_prefix("- ").unwrap_or(line).trim();
        if text.is_empty() {
            continue;
        }
        out.push(Rule {
            line: index + 1,
            text: text.to_string(),
        });
    }
    out
}

/// Backticked identifiers that are shaped like a tool name.
///
/// The shape test matters: `` `anchor│content` `` is backticked prose, not a tool,
/// and the box-drawing character is what rules it out. Anything else backticked
/// that reads like a tool is expected to be one.
pub fn tool_references(rule: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = rule;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let inner = &after[..close];
        if looks_like_tool_name(inner) {
            out.push(inner.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

fn looks_like_tool_name(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    if bytes.len() < 2 || bytes.len() > 32 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// Words with no discriminating power, dropped before comparing two rules.
const STOPWORDS: [&str; 24] = [
    "the", "a", "an", "and", "or", "to", "of", "in", "is", "are", "it", "its", "for", "with",
    "that", "this", "be", "as", "on", "not", "never", "at", "by", "from",
];

/// The significant words of a rule, lowercased.
///
/// Punctuation is dropped rather than tokenised precisely: the comparison only
/// needs to know whether two rules are made of the same words.
fn significant_words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Audit `prompt` against the tools that actually exist.
///
/// Returns every defect found, so one edit that breaks two things reports both.
pub fn audit(prompt: &str, tools: &[&str]) -> Vec<Defect> {
    let rules = rules(prompt);
    if rules.is_empty() {
        return vec![Defect::NoRules];
    }
    let mut defects = Vec::new();

    let tokens = feather_core::tokenizer::estimate_tokens(prompt);
    if tokens > TOKEN_BUDGET {
        defects.push(Defect::OverBudget {
            tokens,
            budget: TOKEN_BUDGET,
        });
    }

    let known: BTreeSet<&str> = tools.iter().copied().collect();
    for rule in &rules {
        for name in tool_references(&rule.text) {
            if !known.contains(name.as_str()) {
                defects.push(Defect::DeadToolReference {
                    line: rule.line,
                    name,
                });
            }
        }
    }

    for (i, a) in rules.iter().enumerate() {
        let wa = significant_words(&a.text);
        for b in rules.iter().skip(i + 1) {
            if a.text.eq_ignore_ascii_case(&b.text) {
                defects.push(Defect::DuplicateRule {
                    first: a.line,
                    second: b.line,
                });
                continue;
            }
            let wb = significant_words(&b.text);
            // Exact set equality is the only duplicate test here that cannot
            // misfire: a fuzzy ratio would eventually flag two genuinely
            // different rules and force the prompt to be reworded to appease it.
            if !wa.is_empty() && wa == wb {
                defects.push(Defect::DuplicateRule {
                    first: a.line,
                    second: b.line,
                });
            }
        }
    }
    containment(&rules, &mut defects);
    defects
}

/// Report rules wholly contained in another rule.
///
/// Verbatim containment is the only redundancy signal here that cannot misfire:
/// the same sentence appearing twice is redundant by definition, whereas a
/// similarity ratio would eventually flag two different rules and pressure the
/// prompt to be reworded to appease the check.
///
/// A 16-character floor keeps short fragments out. Rule text like "prefer
/// `read`" would otherwise turn up inside any sentence that mentions it.
fn containment(rules: &[Rule], defects: &mut Vec<Defect>) {
    const FLOOR: usize = 16;
    for outer in rules {
        let outer_low = normalise(&outer.text);
        let inner = rules.iter().find(|inner| {
            if inner.line == outer.line {
                return false;
            }
            let inner_low = normalise(&inner.text);
            // The floor is applied after normalisation: it exists to stop short
            // fragments matching, and trailing punctuation is not substance.
            inner_low.len() >= FLOOR
                && inner_low.len() < outer_low.len()
                && outer_low.contains(&inner_low)
        });
        if let Some(inner) = inner {
            defects.push(Defect::RuleInsideRule {
                outer: outer.line,
                inner: inner.line,
            });
        }
    }
}

/// Lowercase a rule and strip trailing sentence punctuation.
///
/// Without this, a repeated sentence is missed whenever one copy ends in a full
/// stop and the other is embedded mid-sentence — which is exactly how a rule gets
/// restated in practice.
fn normalise(text: &str) -> String {
    text.trim_end_matches(['.', '!', ';', ':', ','])
        .to_lowercase()
}
