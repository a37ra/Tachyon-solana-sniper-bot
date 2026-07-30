pub mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "solbot-cli")]
#[command(about = "Solana Sniper Bot Administrative CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    KillSwitch {
        #[arg(default_value = "status")]
        action: String,
    },
    Position {
        #[arg(default_value = "list")]
        action: String,
        #[arg(short, long)]
        id: Option<String>,
    },
    CircuitBreaker {
        #[arg(default_value = "status")]
        action: String,
    },
    Bench {
        #[arg(default_value = "all")]
        test: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::KillSwitch { action } => {
            commands::kill_switch::execute(&action).await?;
        }
        Commands::Position { action, id } => {
            commands::position::execute(&action, id.as_deref()).await?;
        }
        Commands::CircuitBreaker { action } => {
            commands::circuit_breaker::execute(&action).await?;
        }
        Commands::Bench { test } => {
            println!("Running latency benchmark test: {}", test);
            // Execute solbot-benchmark binary logic
            let mut child = tokio::process::Command::new("cargo")
                .args(["run", "--bin", "solbot-benchmark", "--", &test])
                .spawn()?;
            child.wait().await?;
        }
    }

    Ok(())
}
