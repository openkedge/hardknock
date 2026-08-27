use clap::Parser;

#[derive(Parser)]
#[command(name = "hardknock", version, about = "Agent experience infrastructure")]
struct Bootstrap {}

fn main() {
    Bootstrap::parse();
}
