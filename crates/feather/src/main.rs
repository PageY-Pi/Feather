//! Feather — one binary coding agent with hashline editing, cache optimization,
//! GUI launcher and headless/RPC compatibility bridge; no interactive TUI.
//! No JS/Lua/Rhai, no DLLs, no runtime code loading anywhere.

use feather::wiring;
use feather::{config, models};

use config::Settings;
use feather_core::agent::{AgentEvent, RunResult};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wiring::*;

struct Cli {
    prompt: Option<String>,
    resume: bool,
    mode: String,
}

fn parse_cli() -> Cli {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cli = Cli {
        prompt: None,
        resume: false,
        mode: "text".into(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => {
                let mode = args.get(i + 1).map(String::as_str).unwrap_or("");
                if !["text", "rpc"].contains(&mode) {
                    eprintln!("--mode requires text or rpc");
                    std::process::exit(2);
                }
                cli.mode = mode.into();
                i += 2;
            }
            "-p" | "--print" => {
                cli.prompt = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "-r" | "--resume" | "-c" | "--continue" => {
                cli.resume = true;
                i += 1;
            }
            "--version" | "-V" => {
                println!("feather {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "feather {} — standalone coding agent in one binary\n\n\
                     USAGE: feather [OPTIONS]\n\n\
                     OPTIONS:\n  \
                     -p, --print <text>  one-shot: answer and exit\n  \
                     -c, -r, --resume    resume the latest session for this directory\n  \
                     --mode rpc         JSONL subprocess protocol (no TUI)\n  \
                     -h, --help          this help\n  \
                     -V, --version       version\n\n\
                     ENV: ANTHROPIC_API_KEY | OPENAI_API_KEY (no key → offline faux model)\n\
                     HOME: FEATHER_HOME (default ~/.feather)",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            other => {
                // positional prompt
                cli.prompt = Some(other.to_string());
                i += 1;
            }
        }
    }
    cli
}

fn retry_delay_label(ms: u64) -> String {
    if ms >= 1_000 {
        let secs = ms as f64 / 1_000.0;
        if (secs.fract()).abs() < f64::EPSILON {
            format!("{}s", secs as u64)
        } else {
            format!("{secs:.1}s")
        }
    } else {
        format!("{ms}ms")
    }
}

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let cli = parse_cli();
    // Apply the persisted traffic limit before any provider request so RPC,
    // one-shot and subagent runs honour the same ceiling as the GUI.
    feather::netlimit::load(&config::feather_home()).apply();
    let root = std::env::current_dir()?;
    if cli.prompt.is_none() && cli.mode != "rpc" {
        // ---------------- interactive (sibling feather-gui) ------------------
        // The interactive experience is a separate binary next to this one; hand
        // it the working directory and wait for it to exit.
        let gui = std::env::current_exe()?
            .parent()
            .map(|dir| {
                dir.join(if cfg!(windows) {
                    "feather-gui.exe"
                } else {
                    "feather-gui"
                })
            })
            .ok_or_else(|| anyhow::anyhow!("cannot locate feather-gui next to feather"))?;
        let mut child = std::process::Command::new(gui).arg(&root).spawn()?;
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("feather-gui exited with {status}");
        }
        return Ok(());
    }
    let proxy_home = config::feather_home();
    feather::proxy::initialize(&proxy_home);
    let proxy_scan_home = proxy_home.clone();
    std::thread::spawn(move || {
        let found = feather::proxy::discover_for(&proxy_scan_home);
        let catalog = feather::proxy::merge_discovered(&proxy_scan_home, found);
        feather_core::provider::transport::set_route(catalog.selected);
    });
    // Refresh provider-published model lists in the background so a slow or
    // offline provider cannot delay this run. The next launch, and every read
    // after the refresh lands, sees the new list.
    feather::model_refresh::spawn_startup_refresh(proxy_home.clone(), || {});
    let mut settings = Settings::load();
    settings.save_default_if_missing();
    // pull pi's model catalog + compaction-model choice (a missing pi install
    // is a no-op); in-memory only — the catalog file is the source of truth
    if settings.compaction_model.is_none() {
        if let Some(cm) = models::load_compaction_model() {
            settings.compaction_model = Some(cm.id);
        }
    }
    let root = std::env::current_dir()?;

    let (mut agent, _tasks) = make_agent(&root, &settings, cli.resume)?;
    if cli.mode == "rpc" {
        if cli.prompt.is_some() {
            anyhow::bail!("RPC mode takes prompts on stdin, not CLI arguments");
        }
        let rpc_settings = std::cell::RefCell::new(settings.clone());
        return feather::rpc::run_with_control(
            agent,
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
            &config::feather_home(),
            &root,
            |session| {
                make_agent_for_session(&root, &rpc_settings.borrow(), session)
                    .map(|(agent, _)| agent)
            },
            |agent, command| model_control(&root, &mut rpc_settings.borrow_mut(), agent, command),
        )
        .await;
    }
    // Host tools are not bundled: warn before the first Bash call fails, so the
    // user gets the fix instead of an opaque tool error. RPC clients (the GUI)
    // render this themselves.
    if let Some(warning) = feather::prereq::warning() {
        eprintln!("feather: {warning}\n");
    }
    let mut events = agent.subscribe();

    if let Some(prompt) = cli.prompt {
        // one-shot mode: stream deltas; print final_text only if nothing streamed
        let streamed = Arc::new(AtomicBool::new(false));
        let streamed2 = streamed.clone();
        let handle = tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => match ev {
                        AgentEvent::TextDelta(t) => {
                            streamed2.store(true, Ordering::Relaxed);
                            print!("{t}");
                        }
                        AgentEvent::ThinkingDelta(_) => {}
                        AgentEvent::ResponseReset {
                            attempt,
                            max_attempts,
                            delay_ms,
                            ..
                        } => {
                            let _ = std::io::stdout().flush();
                            eprintln!(
                                "\n[retry {attempt}/{max_attempts} in {} — previous partial output discarded]",
                                retry_delay_label(delay_ms)
                            );
                        }
                        AgentEvent::Error(e) => eprintln!("\n[error] {e}"),
                        AgentEvent::RunEnd(_) => break,
                        _ => {}
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        let prompt = config::resources_for(&root, &config::feather_home())
            .expand_user_prompt(&prompt)
            .map_err(anyhow::Error::msg)?;
        let res: RunResult = agent
            .prompt_message(feather::mentions::message_from_mentions(&prompt, &root))
            .await?;
        handle.await?;
        if !streamed.load(Ordering::Relaxed) && !res.final_text.trim().is_empty() {
            println!("{}", res.final_text);
        }
        return Ok(());
    }

    Ok(())
}
