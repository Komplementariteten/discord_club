//! Persistence for club member profiles and meeting check-ins (SQLite).

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

/// A club member profile, created by the member before checking in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub guild_id: u64,
    pub user_id: u64,
    pub discord_handle: String,
    pub name: String,
    pub membership_number: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A single check-in, recorded when a member presses the check-in button.
///
/// The profile data is copied into the check-in so that the attendance record
/// stays accurate even if the profile is changed later on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckIn {
    pub id: i64,
    pub guild_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub user_id: u64,
    pub discord_handle: String,
    pub profile_name: String,
    pub membership_number: Option<String>,
    pub checked_in_at: DateTime<Utc>,
}

/// Result of trying to check in a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckInOutcome {
    /// The check-in was stored.
    Recorded(CheckIn),
    /// The member already checked in via this button; the existing record is returned.
    AlreadyCheckedIn(CheckIn),
    /// The member has no profile yet and must create one first.
    NoProfile,
}

/// Details about a button press that should be recorded as a check-in.
#[derive(Debug, Clone)]
pub struct CheckInRequest<'a> {
    pub guild_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub user_id: u64,
    pub discord_handle: &'a str,
    pub at: DateTime<Utc>,
}

pub type Result<T> = rusqlite::Result<T>;

/// Thread-safe SQLite backed storage.
pub struct Storage {
    conn: Mutex<Connection>,
}

// Discord snowflakes are u64 values but always fit into the positive i64 range,
// which is what SQLite stores natively.
fn to_sql_id(id: u64) -> i64 {
    id as i64
}

fn from_sql_id(id: i64) -> u64 {
    id as u64
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn format_time(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

const CHECKIN_COLUMNS: &str = "id, guild_id, channel_id, message_id, user_id, discord_handle, \
     profile_name, membership_number, checked_in_at";

fn checkin_from_row(row: &Row<'_>) -> rusqlite::Result<CheckIn> {
    Ok(CheckIn {
        id: row.get(0)?,
        guild_id: from_sql_id(row.get(1)?),
        channel_id: from_sql_id(row.get(2)?),
        message_id: from_sql_id(row.get(3)?),
        user_id: from_sql_id(row.get(4)?),
        discord_handle: row.get(5)?,
        profile_name: row.get(6)?,
        membership_number: row.get(7)?,
        checked_in_at: parse_time(row.get(8)?)?,
    })
}

impl Storage {
    /// Opens (or creates) the database file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// Creates a throw-away in-memory database (useful for tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS profiles (
                guild_id          INTEGER NOT NULL,
                user_id           INTEGER NOT NULL,
                discord_handle    TEXT    NOT NULL,
                name              TEXT    NOT NULL,
                membership_number TEXT,
                created_at        TEXT    NOT NULL,
                updated_at        TEXT    NOT NULL,
                PRIMARY KEY (guild_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS checkins (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id          INTEGER NOT NULL,
                channel_id        INTEGER NOT NULL,
                message_id        INTEGER NOT NULL,
                user_id           INTEGER NOT NULL,
                discord_handle    TEXT    NOT NULL,
                profile_name      TEXT    NOT NULL,
                membership_number TEXT,
                checked_in_at     TEXT    NOT NULL,
                UNIQUE (message_id, user_id)
            );
            CREATE INDEX IF NOT EXISTS checkins_guild_time ON checkins (guild_id, checked_in_at);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite in an inconsistent state,
        // so it is safe to keep using the connection.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Creates or updates the profile of a member and returns the stored profile.
    pub fn upsert_profile(
        &self,
        guild_id: u64,
        user_id: u64,
        discord_handle: &str,
        name: &str,
        membership_number: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Profile> {
        let conn = self.conn();
        let now = format_time(now);
        conn.execute(
            "INSERT INTO profiles
                (guild_id, user_id, discord_handle, name, membership_number, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT (guild_id, user_id) DO UPDATE SET
                discord_handle = excluded.discord_handle,
                name = excluded.name,
                membership_number = excluded.membership_number,
                updated_at = excluded.updated_at",
            params![
                to_sql_id(guild_id),
                to_sql_id(user_id),
                discord_handle,
                name,
                membership_number,
                now
            ],
        )?;
        Self::query_profile(&conn, guild_id, user_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Returns the profile of a member, if one was created.
    pub fn get_profile(&self, guild_id: u64, user_id: u64) -> Result<Option<Profile>> {
        Self::query_profile(&self.conn(), guild_id, user_id)
    }

    fn query_profile(conn: &Connection, guild_id: u64, user_id: u64) -> Result<Option<Profile>> {
        conn.query_row(
            "SELECT guild_id, user_id, discord_handle, name, membership_number, created_at, updated_at
             FROM profiles WHERE guild_id = ?1 AND user_id = ?2",
            params![to_sql_id(guild_id), to_sql_id(user_id)],
            |row| {
                Ok(Profile {
                    guild_id: from_sql_id(row.get(0)?),
                    user_id: from_sql_id(row.get(1)?),
                    discord_handle: row.get(2)?,
                    name: row.get(3)?,
                    membership_number: row.get(4)?,
                    created_at: parse_time(row.get(5)?)?,
                    updated_at: parse_time(row.get(6)?)?,
                })
            },
        )
        .optional()
    }

    /// Records a check-in for a member pressing the check-in button.
    ///
    /// A member needs a profile to check in, and can check in only once per button
    /// (i.e. per meeting).
    pub fn check_in(&self, request: &CheckInRequest<'_>) -> Result<CheckInOutcome> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        let Some(profile) = Self::query_profile(&tx, request.guild_id, request.user_id)? else {
            return Ok(CheckInOutcome::NoProfile);
        };

        let existing = tx
            .query_row(
                &format!(
                    "SELECT {CHECKIN_COLUMNS} FROM checkins WHERE message_id = ?1 AND user_id = ?2"
                ),
                params![to_sql_id(request.message_id), to_sql_id(request.user_id)],
                checkin_from_row,
            )
            .optional()?;
        if let Some(existing) = existing {
            return Ok(CheckInOutcome::AlreadyCheckedIn(existing));
        }

        tx.execute(
            "INSERT INTO checkins
                (guild_id, channel_id, message_id, user_id, discord_handle,
                 profile_name, membership_number, checked_in_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                to_sql_id(request.guild_id),
                to_sql_id(request.channel_id),
                to_sql_id(request.message_id),
                to_sql_id(request.user_id),
                request.discord_handle,
                profile.name,
                profile.membership_number,
                format_time(request.at),
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;

        Ok(CheckInOutcome::Recorded(CheckIn {
            id,
            guild_id: request.guild_id,
            channel_id: request.channel_id,
            message_id: request.message_id,
            user_id: request.user_id,
            discord_handle: request.discord_handle.to_owned(),
            profile_name: profile.name,
            membership_number: profile.membership_number,
            checked_in_at: request.at,
        }))
    }

    /// Lists all check-ins of a guild on the given (UTC) date, ordered by time.
    pub fn checkins_on(&self, guild_id: u64, date: NaiveDate) -> Result<Vec<CheckIn>> {
        let start = date.and_hms_opt(0, 0, 0).expect("valid time").and_utc();
        let end = start + chrono::Duration::days(1);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CHECKIN_COLUMNS} FROM checkins
             WHERE guild_id = ?1 AND checked_in_at >= ?2 AND checked_in_at < ?3
             ORDER BY checked_in_at, id"
        ))?;
        stmt.query_map(
            params![to_sql_id(guild_id), format_time(start), format_time(end)],
            checkin_from_row,
        )?
        .collect()
    }
}

fn csv_field(value: &str) -> String {
    // Prevent spreadsheet formula injection from user supplied values.
    let value = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

/// Renders check-ins as CSV (UTC timestamps in RFC 3339 format).
pub fn checkins_to_csv(checkins: &[CheckIn]) -> String {
    let mut out = String::from(
        "checked_in_at,name,membership_number,discord_handle,discord_user_id,channel_id,message_id\n",
    );
    for c in checkins {
        let fields = [
            format_time(c.checked_in_at),
            csv_field(&c.profile_name),
            csv_field(c.membership_number.as_deref().unwrap_or("")),
            csv_field(&c.discord_handle),
            c.user_id.to_string(),
            c.channel_id.to_string(),
            c.message_id.to_string(),
        ];
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const GUILD: u64 = 1_100_000_000_000_000_001;
    const USER: u64 = 1_200_000_000_000_000_002;

    fn time(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 21, h, m, 0).unwrap()
    }

    fn request(message_id: u64, at: DateTime<Utc>) -> CheckInRequest<'static> {
        CheckInRequest {
            guild_id: GUILD,
            channel_id: 33,
            message_id,
            user_id: USER,
            discord_handle: "alice",
            at,
        }
    }

    #[test]
    fn profile_create_and_update() {
        let db = Storage::open_in_memory().unwrap();
        assert_eq!(db.get_profile(GUILD, USER).unwrap(), None);

        let created = db
            .upsert_profile(GUILD, USER, "alice", "Alice A.", Some("42"), time(10, 0))
            .unwrap();
        assert_eq!(created.name, "Alice A.");
        assert_eq!(created.membership_number.as_deref(), Some("42"));
        assert_eq!(created.created_at, time(10, 0));

        let updated = db
            .upsert_profile(GUILD, USER, "alice_new", "Alice B.", None, time(11, 0))
            .unwrap();
        assert_eq!(updated.discord_handle, "alice_new");
        assert_eq!(updated.name, "Alice B.");
        assert_eq!(updated.membership_number, None);
        assert_eq!(updated.created_at, time(10, 0));
        assert_eq!(updated.updated_at, time(11, 0));
        assert_eq!(db.get_profile(GUILD, USER).unwrap(), Some(updated));

        // Profiles are scoped per guild.
        assert_eq!(db.get_profile(GUILD + 1, USER).unwrap(), None);
    }

    #[test]
    fn check_in_requires_profile() {
        let db = Storage::open_in_memory().unwrap();
        assert_eq!(
            db.check_in(&request(1, time(18, 0))).unwrap(),
            CheckInOutcome::NoProfile
        );
        assert!(
            db.checkins_on(GUILD, time(0, 0).date_naive())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn check_in_records_time_profile_and_handle() {
        let db = Storage::open_in_memory().unwrap();
        db.upsert_profile(GUILD, USER, "alice", "Alice A.", Some("42"), time(10, 0))
            .unwrap();

        let CheckInOutcome::Recorded(checkin) = db.check_in(&request(1, time(18, 5))).unwrap()
        else {
            panic!("expected check-in to be recorded");
        };
        assert_eq!(checkin.checked_in_at, time(18, 5));
        assert_eq!(checkin.profile_name, "Alice A.");
        assert_eq!(checkin.membership_number.as_deref(), Some("42"));
        assert_eq!(checkin.discord_handle, "alice");
        assert_eq!(checkin.user_id, USER);

        // Pressing the same button again does not create a second record.
        assert_eq!(
            db.check_in(&request(1, time(18, 30))).unwrap(),
            CheckInOutcome::AlreadyCheckedIn(checkin.clone())
        );

        // Profile changes do not alter past check-ins.
        db.upsert_profile(GUILD, USER, "alice", "Renamed", None, time(19, 0))
            .unwrap();
        let listed = db.checkins_on(GUILD, time(0, 0).date_naive()).unwrap();
        assert_eq!(listed, vec![checkin]);
    }

    #[test]
    fn check_in_per_meeting_and_date_filter() {
        let db = Storage::open_in_memory().unwrap();
        db.upsert_profile(GUILD, USER, "alice", "Alice", None, time(10, 0))
            .unwrap();

        assert!(matches!(
            db.check_in(&request(1, time(18, 0))).unwrap(),
            CheckInOutcome::Recorded(_)
        ));
        // Another meeting (button) on the next day.
        let next_day = time(18, 0) + chrono::Duration::days(1);
        assert!(matches!(
            db.check_in(&request(2, next_day)).unwrap(),
            CheckInOutcome::Recorded(_)
        ));

        let today = db.checkins_on(GUILD, time(0, 0).date_naive()).unwrap();
        assert_eq!(today.len(), 1);
        assert_eq!(today[0].message_id, 1);
        let tomorrow = db.checkins_on(GUILD, next_day.date_naive()).unwrap();
        assert_eq!(tomorrow.len(), 1);
        assert_eq!(tomorrow[0].message_id, 2);
        assert!(
            db.checkins_on(GUILD + 1, time(0, 0).date_naive())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn data_survives_reopen() {
        let dir =
            std::env::temp_dir().join(format!("discord_club_test_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        {
            let db = Storage::open(&dir).unwrap();
            db.upsert_profile(GUILD, USER, "alice", "Alice", None, time(10, 0))
                .unwrap();
            db.check_in(&request(1, time(18, 0))).unwrap();
        }
        let db = Storage::open(&dir).unwrap();
        assert!(db.get_profile(GUILD, USER).unwrap().is_some());
        assert_eq!(
            db.checkins_on(GUILD, time(0, 0).date_naive())
                .unwrap()
                .len(),
            1
        );
        drop(db);
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn csv_export_escapes_values() {
        let checkin = CheckIn {
            id: 1,
            guild_id: GUILD,
            channel_id: 3,
            message_id: 4,
            user_id: USER,
            discord_handle: "alice".into(),
            profile_name: "Doe, \"Jane\"".into(),
            membership_number: Some("=1+1".into()),
            checked_in_at: time(18, 5),
        };
        let csv = checkins_to_csv(&[checkin]);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "checked_in_at,name,membership_number,discord_handle,discord_user_id,channel_id,message_id"
        );
        assert_eq!(
            lines.next().unwrap(),
            format!("2026-11-21T18:05:00.000Z,\"Doe, \"\"Jane\"\"\",'=1+1,alice,{USER},3,4")
        );
        assert_eq!(lines.next(), None);
    }
}
