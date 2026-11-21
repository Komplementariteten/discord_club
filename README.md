# discord_club

A Discord bot that adds a **check-in button** to a channel so club members can
check in to a club meeting. Every button press is stored together with

* the date and time of the check-in (UTC),
* the member's previously created club profile (name and optional membership number),
* the member's Discord handle (and user id).

Data is stored in a local SQLite database.

## Commands

| Command | Who | Description |
| --- | --- | --- |
| `/profile name:<full name> [membership_number:<number>]` | everyone | Create or update your member profile. A profile is required before you can check in. |
| `/checkin-button [title:<meeting title>]` | members with *Manage Server* | Posts a message with a **Check in** button in the current channel. |
| `/checkins [date:<YYYY-MM-DD>]` | members with *Manage Server* | Lists the check-ins of a day (UTC, defaults to today) and attaches them as a CSV file. |

Pressing the **Check in** button records the check-in and replies with a
private (ephemeral) confirmation. Each member can check in only once per
button, i.e. once per meeting. Members without a profile are asked to create
one with `/profile` first. Admins can restrict who sees the commands via
*Server Settings → Integrations*.

## Setup

1. Create an application and bot in the [Discord Developer Portal](https://discord.com/developers/applications)
   and copy the bot token.
2. Invite the bot to your server with the `bot` and `applications.commands`
   scopes and the *Send Messages* / *Embed Links* permissions.
3. Run the bot (requires a Rust toolchain):

   ```sh
   export DISCORD_TOKEN=<your bot token>
   # optional:
   export DATABASE_PATH=discord_club.sqlite   # default
   export GUILD_ID=<server id>                # register commands instantly in one server instead of globally
   cargo run --release
   ```

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets
```
