use clap::Parser;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    gongd::run(gongd::Args::parse()).await
}
