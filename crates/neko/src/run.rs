use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use shuru_sdk::{AsyncSandbox, MountConfig, SandboxConfig, ShellEvent};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use crate::commands::now;
use crate::forward;
use crate::login;
use crate::paths;
use crate::store::Store;
use crate::tunnel::{self, Backend, DialRequest};

const WORKSPACE: &str = "/workspace";

pub struct RunArgs {
    pub name: Option<String>,
    pub from: Option<String>,
    pub tunnel: Option<u16>,
    pub ports: Vec<String>,
    pub subdomain: Option<String>,
    pub private: Option<String>,
    pub term: bool,
    pub checkpoint: Option<Option<String>>,
    pub command: Vec<String>,
}

pub async fn run(args: RunArgs) -> Result<()> {
    // Reject a bad --port before spending a boot on it.
    let mappings = args
        .ports
        .iter()
        .map(|spec| forward::parse(spec))
        .collect::<Result<Vec<_>>>()?;

    if args.private.is_some() && args.tunnel.is_none() && !args.term {
        bail!("--private requires --tunnel or --term");
    }
    // A web terminal is a shell into the sandbox; it only ever rides a
    // tunnel whose visitors are authenticated. Bare --term means the
    // personal workspace, exactly like bare --private.
    let private = match (&args.private, args.term) {
        (Some(selector), _) => Some(selector.clone()),
        (None, true) => Some(String::new()),
        (None, false) => None,
    };
    let opens_tunnel = args.tunnel.is_some() || args.term;

    let data_dir = paths::ensure_ready().await?;
    let store = Store::open(&paths::db_path()?)?;

    // Fetch the tunnel token before booting so an unauthenticated run fails
    // fast. Local forwarding needs no account.
    let token = match opens_tunnel {
        true => Some(login::access_jwt().await?),
        false => None,
    };
    let audience = match &private {
        None => None,
        Some(selector) => Some(login::resolve_workspace(selector).await?),
    };

    let existing = match &args.name {
        Some(n) => store.get_computer(n)?,
        None => None,
    };

    // Boot source and the parent for any new checkpoint.
    let (boot_from, parent): (Option<String>, Option<String>) = if let Some(reference) = &args.from
    {
        if existing.as_ref().and_then(|c| c.head_id.as_ref()).is_some() {
            bail!("--from cannot seed an existing computer with history; use a new name");
        }
        let cp = store.resolve_ref(reference)?;
        (Some(cp.id.clone()), Some(cp.id))
    } else if let Some(c) = &existing {
        (c.head_id.clone(), c.head_id.clone())
    } else {
        (None, None)
    };

    if let Some(n) = &args.name {
        if existing.is_none() {
            store.create_computer(n, boot_from.as_deref(), now())?;
        }
    }

    println!("booting sandbox...");
    let sandbox = Rc::new(
        AsyncSandbox::boot(SandboxConfig {
            allow_net: true,
            data_dir: Some(data_dir.to_string_lossy().into_owned()),
            from: boot_from,
            mounts: vec![MountConfig {
                host_path: std::env::current_dir()?.to_string_lossy().into_owned(),
                guest_path: WORKSPACE.to_string(),
                read_only: false,
            }],
            ..Default::default()
        })
        .await?,
    );

    // Start the command, stream output, signal when it exits.
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let _server = if args.command.is_empty() {
        None
    } else {
        let argv: Vec<&str> = args.command.iter().map(|s| s.as_str()).collect();
        let handle = sandbox
            .open_shell(24, 80, Some(WORKSPACE), Some(&argv), HashMap::new())
            .await?;
        println!("started: {}", args.command.join(" "));
        let (writer, mut reader) = handle.split();
        tokio::spawn(async move {
            let mut done_tx = Some(done_tx);
            while let Some(event) = reader.recv().await {
                match event {
                    ShellEvent::Output(bytes) => {
                        let _ = tokio::io::stderr().write_all(&bytes).await;
                    }
                    ShellEvent::Exit(code) => {
                        eprintln!("neko: command exited with code {code}");
                        if let Some(tx) = done_tx.take() {
                            let _ = tx.send(());
                        }
                        break;
                    }
                    ShellEvent::Error(err) => {
                        eprintln!("neko: command error: {err}");
                        if let Some(tx) = done_tx.take() {
                            let _ = tx.send(());
                        }
                        break;
                    }
                }
            }
        });
        Some(writer)
    };

    // The sandbox is not Sync; one LocalSet task owns it and answers dials
    // and web-terminal shell requests.
    let (dial_tx, mut dial_rx) = mpsc::unbounded_channel::<DialRequest>();
    let (shell_tx, mut shell_rx) = mpsc::unbounded_channel::<tunnel::ShellRequest>();
    let local = LocalSet::new();
    {
        let sb = sandbox.clone();
        local.spawn_local(async move {
            let mut shells_open = true;
            loop {
                tokio::select! {
                    req = dial_rx.recv() => match req {
                        Some((port, reply)) => {
                            let stream = sb.dial_guest(port).await.map(|g| g.into_inner());
                            let _ = reply.send(stream);
                        }
                        None => break,
                    },
                    req = shell_rx.recv(), if shells_open => match req {
                        Some((rows, cols, reply)) => {
                            let shell = sb
                                .open_shell(rows, cols, Some(WORKSPACE), None, HashMap::new())
                                .await
                                .map(|h| h.split());
                            let _ = reply.send(shell);
                        }
                        None => shells_open = false,
                    },
                }
            }
        });
    }

    // With --tunnel run the tunnel; otherwise wait for the command. Ctrl-c ends either.
    local
        .run_until(async {
            let ctrl_c = tokio::signal::ctrl_c();

            // Local forwarding runs alongside anything else, including a tunnel.
            for map in &mappings {
                match forward::start(*map, dial_tx.clone()).await {
                    Ok(()) => println!(
                        "forwarding http://127.0.0.1:{} -> sandbox:{}",
                        map.host, map.guest
                    ),
                    Err(e) => eprintln!("{e}"),
                }
            }
            if !mappings.is_empty() && args.tunnel.is_none() {
                println!("current dir mounted at {WORKSPACE}");
                println!("press ctrl-c to stop");
            }

            if opens_tunnel {
                let port = args.tunnel;
                let claimed = args.subdomain.is_some();
                let sub = args
                    .subdomain
                    .clone()
                    .unwrap_or_else(crate::names::random_name);
                let domain = crate::dist::domain();
                let org = audience.as_ref().map(|(id, _)| id.as_str());
                let who = audience.as_ref().map(|(_, label)| label.clone());
                let term = args.term;
                let announce = || {
                    let suffix = match &who {
                        Some(who) => format!(" (private: {who})"),
                        None => String::new(),
                    };
                    if let Some(port) = port {
                        println!(
                            "(=^..^=)つ tunnel open: https://{sub}.{domain} -> sandbox:{port}{suffix}"
                        );
                    }
                    if term {
                        println!("(=^..^=)つ terminal: https://{sub}.{domain}/__neko/term{suffix}");
                    }
                    println!("current dir mounted at {WORKSPACE}");
                    println!("press ctrl-c to stop");
                };
                let serve = tunnel::run(
                    &sub,
                    claimed,
                    Backend::Sandbox(dial_tx, port.unwrap_or(0)),
                    token.as_deref().unwrap_or_default(),
                    org,
                    args.term.then(|| tunnel::TermBridge {
                        shells: shell_tx.clone(),
                        exclusive: port.is_none(),
                    }),
                    announce,
                );
                tokio::select! {
                    outcome = serve => {
                        if let Err(e) = outcome {
                            eprintln!("tunnel closed: {e}");
                        }
                    }
                    _ = done_rx => {}
                    _ = ctrl_c => {}
                }
            } else {
                tokio::select! {
                    _ = done_rx => {}
                    _ = ctrl_c => {}
                }
            }
        })
        .await;

    // Checkpoint on the way down: flush the guest fs, then snapshot.
    if let Some(label) = args.checkpoint {
        let name = args
            .name
            .as_deref()
            .context("--checkpoint requires a computer name")?;
        let id = Store::new_checkpoint_id();
        let _ = sandbox.exec(&["sync"]).await;
        sandbox.checkpoint(&id).await?;
        let image = paths::checkpoint_image(&id)?.to_string_lossy().into_owned();
        store.add_checkpoint(&id, parent.as_deref(), label.as_deref(), &image, now())?;
        store.set_head(name, &id)?;
        match &label {
            Some(l) => println!("checkpoint {} ({l}) saved for {name}", &id[..8]),
            None => println!("checkpoint {} saved for {name}", &id[..8]),
        }
    }

    Ok(())
}
