//! Discord integration: slash commands and the check-in button.

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use serenity::all::{
    ButtonStyle, Command, CommandInteraction, CommandOptionType, ComponentInteraction, Context,
    CreateActionRow, CreateAttachment, CreateButton, CreateCommand, CreateCommandOption,
    CreateEmbed, CreateInteractionResponse, CreateInteractionResponseMessage, EventHandler,
    GuildId, Interaction, Permissions, Ready, ResolvedValue,
};
use serenity::async_trait;
use tracing::{error, info, warn};

use crate::storage::{CheckIn, CheckInOutcome, CheckInRequest, Storage, checkins_to_csv};

/// Custom id of the check-in button. It is static so buttons keep working after restarts.
pub const CHECKIN_BUTTON_ID: &str = "club_checkin";

pub const CMD_PROFILE: &str = "profile";
pub const CMD_CHECKIN_BUTTON: &str = "checkin-button";
pub const CMD_CHECKINS: &str = "checkins";

const MAX_NAME_LEN: u16 = 100;
const MAX_MEMBERSHIP_NUMBER_LEN: u16 = 50;
const MAX_TITLE_LEN: u16 = 200;
/// Discord rejects messages with more than 2000 characters. The `/checkins` reply lists
/// check-ins inline only up to this length (all check-ins are in the attached CSV file),
/// leaving room for the "… and N more" line.
const MAX_SUMMARY_CHARS: usize = 1900;

pub struct Handler {
    storage: Arc<Storage>,
    guild_id: Option<GuildId>,
}

impl Handler {
    /// `guild_id`: register commands only in this guild (instant), otherwise globally.
    pub fn new(storage: Arc<Storage>, guild_id: Option<GuildId>) -> Self {
        Self { storage, guild_id }
    }
}

/// Definitions of all slash commands provided by the bot.
pub fn commands() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new(CMD_PROFILE)
            .description("Create or update your club member profile (required to check in)")
            .dm_permission(false)
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "name", "Your full name")
                    .required(true)
                    .max_length(MAX_NAME_LEN),
            )
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "membership_number",
                    "Your club membership number",
                )
                .required(false)
                .max_length(MAX_MEMBERSHIP_NUMBER_LEN),
            ),
        CreateCommand::new(CMD_CHECKIN_BUTTON)
            .description("Post a check-in button for a club meeting in this channel")
            .dm_permission(false)
            .default_member_permissions(Permissions::MANAGE_GUILD)
            .add_option(
                CreateCommandOption::new(CommandOptionType::String, "title", "Meeting title")
                    .required(false)
                    .max_length(MAX_TITLE_LEN),
            ),
        CreateCommand::new(CMD_CHECKINS)
            .description("Show the check-ins of a day (with CSV export)")
            .dm_permission(false)
            .default_member_permissions(Permissions::MANAGE_GUILD)
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "date",
                    "Date in YYYY-MM-DD format (UTC), defaults to today",
                )
                .required(false),
            ),
    ]
}

/// Parses the optional `date` option of `/checkins`.
pub fn parse_date(value: Option<&str>, today: NaiveDate) -> Result<NaiveDate, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(today),
        Some(v) => NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .map_err(|_| format!("Invalid date `{v}`, please use the format YYYY-MM-DD.")),
    }
}

/// Validates and normalizes free text options, returning `None` if empty.
pub fn clean_text(value: Option<&str>) -> Option<String> {
    value
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|v| !v.is_empty())
}

fn discord_time(checkin: &CheckIn) -> String {
    format!("<t:{}:F>", checkin.checked_in_at.timestamp())
}

/// The (ephemeral) reply shown to a member after pressing the check-in button.
pub fn checkin_reply(outcome: &CheckInOutcome) -> String {
    match outcome {
        CheckInOutcome::Recorded(c) => format!(
            "✅ Checked in as **{}** at {}. Welcome!",
            c.profile_name,
            discord_time(c)
        ),
        CheckInOutcome::AlreadyCheckedIn(c) => format!(
            "ℹ️ You already checked in to this meeting as **{}** at {}.",
            c.profile_name,
            discord_time(c)
        ),
        CheckInOutcome::NoProfile => format!(
            "⚠️ You need a member profile before you can check in. \
             Create one with `/{CMD_PROFILE}` and press the button again."
        ),
    }
}

/// Summary text for `/checkins`.
pub fn checkins_summary(date: NaiveDate, checkins: &[CheckIn]) -> String {
    if checkins.is_empty() {
        return format!("No check-ins on {date} (UTC).");
    }
    let mut out = format!("**{} check-in(s) on {date} (UTC):**\n", checkins.len());
    let mut out_chars = out.chars().count();
    let mut listed = 0;
    for c in checkins {
        let number = c
            .membership_number
            .as_deref()
            .map(|n| format!(" (#{n})"))
            .unwrap_or_default();
        let line = format!(
            "• {} — {}{} — <@{}> (`{}`)\n",
            discord_time(c),
            c.profile_name,
            number,
            c.user_id,
            c.discord_handle
        );
        let line_chars = line.chars().count();
        if out_chars + line_chars > MAX_SUMMARY_CHARS {
            break;
        }
        out.push_str(&line);
        out_chars += line_chars;
        listed += 1;
    }
    if listed < checkins.len() {
        out.push_str(&format!(
            "… and {} more, see the attached CSV file.\n",
            checkins.len() - listed
        ));
    }
    out
}

fn string_option<'a>(command: &'a CommandInteraction, name: &str) -> Option<&'a str> {
    command
        .data
        .options()
        .into_iter()
        .find(|o| o.name == name)
        .and_then(|o| match o.value {
            ResolvedValue::String(s) => Some(s),
            _ => None,
        })
}

fn ephemeral(content: impl Into<String>) -> CreateInteractionResponseMessage {
    CreateInteractionResponseMessage::new()
        .content(content)
        .ephemeral(true)
}

const INTERNAL_ERROR: &str = "❌ Something went wrong, please try again later.";

impl Handler {
    fn handle_profile(
        &self,
        command: &CommandInteraction,
        guild_id: GuildId,
    ) -> CreateInteractionResponseMessage {
        let Some(name) = clean_text(string_option(command, "name")) else {
            return ephemeral("Please provide your name.");
        };
        let membership_number = clean_text(string_option(command, "membership_number"));
        match self.storage.upsert_profile(
            guild_id.get(),
            command.user.id.get(),
            &command.user.tag(),
            &name,
            membership_number.as_deref(),
            Utc::now(),
        ) {
            Ok(profile) => {
                let number = profile
                    .membership_number
                    .map(|n| format!(", membership number **{n}**"))
                    .unwrap_or_default();
                ephemeral(format!(
                    "✅ Profile saved: **{}**{number} (`{}`). You can now check in to club meetings.",
                    profile.name, profile.discord_handle
                ))
            }
            Err(e) => {
                error!("failed to save profile: {e}");
                ephemeral(INTERNAL_ERROR)
            }
        }
    }

    fn handle_checkin_button(
        &self,
        command: &CommandInteraction,
    ) -> CreateInteractionResponseMessage {
        let title = clean_text(string_option(command, "title"))
            .unwrap_or_else(|| "Club meeting".to_owned());
        let embed = CreateEmbed::new()
            .title(format!("📋 {title}"))
            .description(format!(
                "Press the button below to check in to this meeting.\n\
                 You need a member profile first — create one with `/{CMD_PROFILE}`."
            ))
            .timestamp(serenity::model::Timestamp::now());
        let button = CreateButton::new(CHECKIN_BUTTON_ID)
            .label("Check in")
            .emoji('✅')
            .style(ButtonStyle::Success);
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(vec![button])])
    }

    fn handle_checkins(
        &self,
        command: &CommandInteraction,
        guild_id: GuildId,
    ) -> CreateInteractionResponseMessage {
        let date = match parse_date(string_option(command, "date"), Utc::now().date_naive()) {
            Ok(date) => date,
            Err(msg) => return ephemeral(msg),
        };
        match self.storage.checkins_on(guild_id.get(), date) {
            Ok(checkins) => {
                let mut response = ephemeral(checkins_summary(date, &checkins));
                if !checkins.is_empty() {
                    response = response.add_file(CreateAttachment::bytes(
                        checkins_to_csv(&checkins).into_bytes(),
                        format!("checkins-{date}.csv"),
                    ));
                }
                response
            }
            Err(e) => {
                error!("failed to load check-ins: {e}");
                ephemeral(INTERNAL_ERROR)
            }
        }
    }

    async fn handle_command(&self, ctx: &Context, command: &CommandInteraction) {
        let response = match command.guild_id {
            None => ephemeral("This command can only be used in a server."),
            Some(guild_id) => match command.data.name.as_str() {
                CMD_PROFILE => self.handle_profile(command, guild_id),
                CMD_CHECKIN_BUTTON => self.handle_checkin_button(command),
                CMD_CHECKINS => self.handle_checkins(command, guild_id),
                other => {
                    warn!("unknown command: {other}");
                    ephemeral("Unknown command.")
                }
            },
        };
        if let Err(e) = command
            .create_response(&ctx.http, CreateInteractionResponse::Message(response))
            .await
        {
            error!("failed to respond to command: {e}");
        }
    }

    async fn handle_component(&self, ctx: &Context, component: &ComponentInteraction) {
        if component.data.custom_id != CHECKIN_BUTTON_ID {
            return;
        }
        let content = match component.guild_id {
            None => "Check-ins are only possible in a server.".to_owned(),
            Some(guild_id) => {
                let handle = component.user.tag();
                let request = CheckInRequest {
                    guild_id: guild_id.get(),
                    channel_id: component.channel_id.get(),
                    message_id: component.message.id.get(),
                    user_id: component.user.id.get(),
                    discord_handle: &handle,
                    at: Utc::now(),
                };
                match self.storage.check_in(&request) {
                    Ok(outcome) => {
                        if let CheckInOutcome::Recorded(c) = &outcome {
                            info!(
                                "{} ({}) checked in at {}",
                                c.profile_name, c.discord_handle, c.checked_in_at
                            );
                        }
                        checkin_reply(&outcome)
                    }
                    Err(e) => {
                        error!("failed to record check-in: {e}");
                        INTERNAL_ERROR.to_owned()
                    }
                }
            }
        };
        if let Err(e) = component
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(ephemeral(content)),
            )
            .await
        {
            error!("failed to respond to button press: {e}");
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("connected as {}", ready.user.name);
        let result = match self.guild_id {
            Some(guild_id) => guild_id.set_commands(&ctx.http, commands()).await,
            None => Command::set_global_commands(&ctx.http, commands()).await,
        };
        match result {
            Ok(cmds) => info!("registered {} slash command(s)", cmds.len()),
            Err(e) => error!("failed to register slash commands: {e}"),
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => self.handle_command(&ctx, &command).await,
            Interaction::Component(component) => self.handle_component(&ctx, &component).await,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn checkin(name: &str, number: Option<&str>) -> CheckIn {
        CheckIn {
            id: 1,
            guild_id: 1,
            channel_id: 2,
            message_id: 3,
            user_id: 4,
            discord_handle: "alice".into(),
            profile_name: name.into(),
            membership_number: number.map(Into::into),
            checked_in_at: Utc.with_ymd_and_hms(2026, 11, 21, 18, 5, 0).unwrap(),
        }
    }

    #[test]
    fn parse_date_defaults_to_today() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        assert_eq!(parse_date(None, today), Ok(today));
        assert_eq!(parse_date(Some("  "), today), Ok(today));
        assert_eq!(
            parse_date(Some("2026-01-02"), today),
            Ok(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap())
        );
        assert!(parse_date(Some("21.11.2026"), today).is_err());
    }

    #[test]
    fn clean_text_normalizes_whitespace() {
        assert_eq!(clean_text(Some("  Jane   Doe ")), Some("Jane Doe".into()));
        assert_eq!(clean_text(Some("   ")), None);
        assert_eq!(clean_text(None), None);
    }

    #[test]
    fn checkin_replies() {
        let c = checkin("Alice", None);
        let ts = c.checked_in_at.timestamp();
        assert_eq!(
            checkin_reply(&CheckInOutcome::Recorded(c.clone())),
            format!("✅ Checked in as **Alice** at <t:{ts}:F>. Welcome!")
        );
        assert!(checkin_reply(&CheckInOutcome::AlreadyCheckedIn(c)).contains("already checked in"));
        assert!(checkin_reply(&CheckInOutcome::NoProfile).contains("/profile"));
    }

    #[test]
    fn summary_lists_checkins() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        assert_eq!(
            checkins_summary(date, &[]),
            "No check-ins on 2026-11-21 (UTC)."
        );

        let summary = checkins_summary(date, &[checkin("Alice", Some("42"))]);
        assert!(summary.starts_with("**1 check-in(s) on 2026-11-21 (UTC):**"));
        assert!(summary.contains("Alice (#42) — <@4> (`alice`)"));

        // Long lists are truncated to stay below Discord's 2000 character limit.
        let long_name = "N".repeat(100);
        let many: Vec<_> = (0..30)
            .map(|_| checkin(&long_name, Some(&"9".repeat(50))))
            .collect();
        let summary = checkins_summary(date, &many);
        assert!(summary.chars().count() <= 2000);
        let listed = summary.matches(&long_name).count();
        assert!(listed > 0 && listed < 30);
        assert!(summary.contains(&format!("… and {} more", 30 - listed)));
    }

    #[test]
    fn commands_are_defined() {
        assert_eq!(commands().len(), 3);
    }
}
