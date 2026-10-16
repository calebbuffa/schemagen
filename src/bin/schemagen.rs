use clap::Parser;
use schemagen::{DefaultPolicy, driver};

#[derive(Parser)]
#[command(
    name = "schemagen",
    version,
    about = "Generate Rust types from JSON Schema"
)]
struct Cli {
    #[command(flatten)]
    args: driver::Args,
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = driver::run(&cli.args, &DefaultPolicy) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
