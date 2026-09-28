//! User settings (`user_settings`): one row per user, created by the first [`save`].

use sqlx::{PgPool, types::JsonValue};

use super::{
    error::{RepoError, narrow},
    ids::UserId,
};

/// The unit weights are shown and entered in. Weights are always stored in nanograms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Kg,
    Lb,
}

impl Unit {
    /// The stored text.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kg => "kg",
            Self::Lb => "lb",
        }
    }

    fn parse(value: &str) -> Result<Self, RepoError> {
        match value {
            "kg" => Ok(Self::Kg),
            "lb" => Ok(Self::Lb),
            _ => Err(RepoError::Corrupt("user_settings.unit")),
        }
    }
}

/// A user's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct UserSettings {
    pub unit: Unit,
    /// Bar weight, in nanograms (the domain `Weight`).
    pub bar_weight_ng: u64,
    /// The domain `PlateInventory` as JSON (an array). Validate it with the domain before saving.
    pub plate_inventory: JsonValue,
    /// Default rest between sets, in seconds (the domain `Seconds`).
    pub default_rest_s: u32,
    pub sound_enabled: bool,
}

impl UserSettings {
    /// The settings of a user who never saved any. Must match the column defaults of
    /// `user_settings` (a test checks it).
    pub fn defaults() -> Self {
        Self {
            unit: Unit::Kg,
            bar_weight_ng: 20_000_000_000_000,
            plate_inventory: JsonValue::Array(Vec::new()),
            default_rest_s: 120,
            sound_enabled: true,
        }
    }
}

/// The user's settings, or [`UserSettings::defaults`] if they never saved any.
pub async fn get(pool: &PgPool, user: UserId) -> Result<UserSettings, RepoError> {
    let row = sqlx::query!(
        "SELECT unit, bar_weight_ng, plate_inventory, default_rest_s, sound_enabled
         FROM user_settings WHERE user_id = $1",
        user.as_uuid()
    )
    .fetch_optional(pool)
    .await?;
    match row {
        None => Ok(UserSettings::defaults()),
        Some(row) => Ok(UserSettings {
            unit: Unit::parse(&row.unit)?,
            bar_weight_ng: narrow(row.bar_weight_ng, "user_settings.bar_weight_ng")?,
            plate_inventory: row.plate_inventory,
            default_rest_s: narrow(row.default_rest_s, "user_settings.default_rest_s")?,
            sound_enabled: row.sound_enabled,
        }),
    }
}

/// Saves the user's settings, replacing the previous ones.
///
/// # Errors
/// [`RepoError::Invalid`] when a value is out of range (bar weight above 2000 kg, a plate inventory
/// that is not an array of at most 16 entries).
pub async fn save(pool: &PgPool, user: UserId, settings: &UserSettings) -> Result<(), RepoError> {
    let bar_weight_ng = i64::try_from(settings.bar_weight_ng).map_err(|_| RepoError::Invalid {
        constraint: Some("user_settings_bar_weight_ng_check".to_owned()),
    })?;
    sqlx::query!(
        "INSERT INTO user_settings
             (user_id, unit, bar_weight_ng, plate_inventory, default_rest_s, sound_enabled)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id) DO UPDATE SET
             unit = EXCLUDED.unit,
             bar_weight_ng = EXCLUDED.bar_weight_ng,
             plate_inventory = EXCLUDED.plate_inventory,
             default_rest_s = EXCLUDED.default_rest_s,
             sound_enabled = EXCLUDED.sound_enabled,
             updated_at = now()",
        user.as_uuid(),
        settings.unit.as_str(),
        bar_weight_ng,
        settings.plate_inventory,
        i64::from(settings.default_rest_s),
        settings.sound_enabled,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{MIGRATOR, testing};
    use serde_json::json;

    fn custom() -> UserSettings {
        UserSettings {
            unit: Unit::Lb,
            bar_weight_ng: 15_000_000_000_000,
            plate_inventory: json!([{"plate": 20.0, "pairs": 4}]),
            default_rest_s: u32::MAX,
            sound_enabled: false,
        }
    }

    #[test]
    fn unit_text_round_trips_and_rejects_unknown_values() {
        for unit in [Unit::Kg, Unit::Lb] {
            assert_eq!(Unit::parse(unit.as_str()).unwrap(), unit);
        }
        assert!(matches!(Unit::parse("st"), Err(RepoError::Corrupt(_))));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_user_without_settings_gets_the_defaults(pool: PgPool) {
        let user = testing::user(&pool).await;
        assert_eq!(get(&pool, user).await.unwrap(), UserSettings::defaults());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn rust_defaults_match_the_column_defaults(pool: PgPool) {
        let user = testing::user(&pool).await;
        sqlx::query!(
            "INSERT INTO user_settings (user_id) VALUES ($1)",
            user.as_uuid()
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(get(&pool, user).await.unwrap(), UserSettings::defaults());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn save_then_get_round_trips_and_save_replaces(pool: PgPool) {
        let user = testing::user(&pool).await;
        save(&pool, user, &custom()).await.unwrap();
        assert_eq!(get(&pool, user).await.unwrap(), custom());
        save(&pool, user, &UserSettings::defaults()).await.unwrap();
        assert_eq!(get(&pool, user).await.unwrap(), UserSettings::defaults());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn out_of_range_values_are_rejected(pool: PgPool) {
        let user = testing::user(&pool).await;
        let max = UserSettings {
            bar_weight_ng: 2_000_000_000_000_000,
            ..UserSettings::defaults()
        };
        save(&pool, user, &max).await.unwrap();
        for bad in [
            UserSettings {
                bar_weight_ng: 2_000_000_000_000_001,
                ..UserSettings::defaults()
            },
            UserSettings {
                bar_weight_ng: u64::MAX,
                ..UserSettings::defaults()
            },
            UserSettings {
                plate_inventory: json!({"plate": 20}),
                ..UserSettings::defaults()
            },
            UserSettings {
                plate_inventory: JsonValue::Array(vec![json!({}); 17]),
                ..UserSettings::defaults()
            },
        ] {
            let error = save(&pool, user, &bad).await.unwrap_err();
            assert!(matches!(error, RepoError::Invalid { .. }), "{error:?}");
        }
        assert_eq!(get(&pool, user).await.unwrap(), max);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn users_only_see_and_change_their_own_settings(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        save(&pool, a, &custom()).await.unwrap();
        // B sees the defaults, not A's settings.
        assert_eq!(get(&pool, b).await.unwrap(), UserSettings::defaults());
        // B saving creates B's row and leaves A's alone.
        save(&pool, b, &UserSettings::defaults()).await.unwrap();
        assert_eq!(get(&pool, a).await.unwrap(), custom());
    }
}
