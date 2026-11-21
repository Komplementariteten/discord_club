use std::sync::Arc;

use discord_club::bot::Handler;
use discord_club::storage::Storage;
use serenity::all::{Client, GatewayIntents, GuildId};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let token =
        std::env::var("DISCORD_TOKEN").expect("the DISCORD_TOKEN environment variable must be set");
    let database_path =
        std::env::var("DATABASE_PATH").unwrap_or_else(|_| "discord_club.sqlite".to_owned());
    let guild_id = std::env::var("GUILD_ID").ok().map(|id| {
        GuildId::new(
            id.trim()
                .parse()
                .expect("GUILD_ID must be a valid Discord server id"),
        )
    });

    let storage = Storage::open(&database_path)
        .unwrap_or_else(|e| panic!("failed to open database {database_path}: {e}"));

    let mut client = Client::builder(&token, GatewayIntents::GUILDS)
        .event_handler(Handler::new(Arc::new(storage), guild_id))
        .await
        .expect("failed to create Discord client");

    if let Err(e) = client.start().await {
        eprintln!("client error: {e}");
        std::process::exit(1);
    }
}
