mod commands;
mod dist;
mod forward;
mod github;
mod login;
mod mascot;
mod names;
mod paths;
mod run;
mod store;
mod term;
mod tunnel;
mod upgrade;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "neko",
    version,
    about = "Expose shuru sandbox computers to the web"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in via the SuperHQ device grant.
    Login,
    /// Boot a computer, run a command, optionally tunnel a port and checkpoint.
    Run {
        name: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        tunnel: Option<u16>,
        /// Forward a sandbox port to your machine: --port 8000, or --port
        /// 3000:8000 for host:guest. Repeatable. No account needed.
        #[arg(long = "port", value_name = "[HOST:]GUEST")]
        ports: Vec<String>,
        #[arg(long)]
        subdomain: Option<String>,
        /// Only workspace members may visit the tunnel; they sign in with
        /// their SuperHQ account. Bare --private means your personal
        /// workspace, or pass a workspace slug.
        #[arg(long, value_name = "WORKSPACE", num_args = 0..=1, default_missing_value = "")]
        private: Option<String>,
        /// Serve a web terminal into the sandbox at /__neko/term. Opens a
        /// private tunnel on its own (personal workspace unless --private
        /// names one); add --tunnel to also expose an app port.
        #[arg(long)]
        term: bool,
        #[arg(long, num_args = 0..=1)]
        checkpoint: Option<Option<String>>,
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Tunnel a local host port to the web (no sandbox).
    Tunnel {
        port: u16,
        #[arg(long)]
        subdomain: Option<String>,
        /// Only workspace members may visit; they sign in with their SuperHQ
        /// account. Bare --private means your personal workspace, or pass a
        /// workspace slug.
        #[arg(long, value_name = "WORKSPACE", num_args = 0..=1, default_missing_value = "")]
        private: Option<String>,
    },
    /// Create a computer from a ref or an image file, without running it.
    Clone { source: String, name: String },
    /// List computers.
    Ls,
    /// Show the checkpoint history of a computer.
    History { name: String },
    /// Remove a computer and reclaim its orphaned checkpoints.
    Rm {
        name: String,
        #[arg(long)]
        keep_checkpoints: bool,
    },
    /// Prune checkpoints unreachable from any computer.
    Gc,
    /// Upgrade neko to the latest release.
    Upgrade,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    match Cli::parse().command {
        Command::Login => login::login().await,
        Command::Run {
            name,
            from,
            tunnel,
            ports,
            subdomain,
            private,
            term,
            checkpoint,
            command,
        } => {
            run::run(run::RunArgs {
                name,
                from,
                tunnel,
                ports,
                subdomain,
                private,
                term,
                checkpoint,
                command,
            })
            .await
        }
        Command::Tunnel {
            port,
            subdomain,
            private,
        } => {
            let token = login::access_jwt().await?;
            let audience = match private {
                None => None,
                Some(selector) => Some(login::resolve_workspace(&selector).await?),
            };
            let claimed = subdomain.is_some();
            let sub = subdomain.unwrap_or_else(names::random_name);
            let domain = dist::domain();
            let org = audience.as_ref().map(|(id, _)| id.as_str());
            let label = audience.as_ref().map(|(_, label)| label.clone());
            let cat = mascot::Mascot::start("opening tunnel...");
            let ready_cat = cat.clone();
            let outcome = tunnel::run(&sub, claimed, tunnel::Backend::Local(port), &token, org, None, || {
                ready_cat.finish();
                match &label {
                    Some(who) => {
                        println!("(=^..^=)つ tunnel open: https://{sub}.{domain} -> 127.0.0.1:{port} (private: {who})")
                    }
                    None => println!("(=^..^=)つ tunnel open: https://{sub}.{domain} -> 127.0.0.1:{port} (public)"),
                }
                println!("waiting for requests, press ctrl-c to stop");
            })
            .await;
            cat.finish();
            outcome
        }
        Command::Clone { source, name } => commands::clone(&source, &name),
        Command::Ls => commands::ls(),
        Command::History { name } => commands::history(&name),
        Command::Rm {
            name,
            keep_checkpoints,
        } => commands::rm(&name, keep_checkpoints),
        Command::Gc => commands::gc(),
        Command::Upgrade => upgrade::upgrade().await,
    }
}

#[cfg(test)]
#[path = "forward_tests.rs"]
mod forward_tests;
