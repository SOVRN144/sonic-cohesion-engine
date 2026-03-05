use anyhow::{bail, Context};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row,
};
use std::str::FromStr;

#[derive(Clone)]
pub struct Db {
    pool: sqlx::SqlitePool,
}

impl Db {
    pub async fn connect(db_url: &str) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::from_str(db_url)
            .with_context(|| format!("parse sqlite url: {db_url}"))?;

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON;")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .with_context(|| format!("connect sqlite at {db_url}"))?;

        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }
}

pub async fn enforce_startup_invariants(db: &Db) -> anyhow::Result<()> {
    let sqlite_version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(db.pool())
        .await?;

    let parsed = parse_sqlite_version(&sqlite_version)?;
    tracing::info!(sqlite_version = %sqlite_version, "detected sqlite version");

    // Hard-fail: atomic queue claim depends on UPDATE ... RETURNING (SQLite >= 3.35.0)
    if parsed < (3, 35, 0) {
        bail!(
            "sqlite version {sqlite_version} is too old; required >= 3.35.0 for UPDATE ... RETURNING"
        );
    }

    let fk_enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(db.pool())
        .await?;
    if fk_enabled != 1 {
        bail!("PRAGMA foreign_keys must be 1, got {fk_enabled}");
    }

    let index_rows = sqlx::query("PRAGMA index_list('analysis_runs')")
        .fetch_all(db.pool())
        .await?;

    let has_queue_index = index_rows.iter().any(|row| {
        row.try_get::<String, _>("name")
            .map(|name| name == "idx_runs_status_queuedat_id")
            .unwrap_or(false)
    });

    if !has_queue_index {
        bail!("required index idx_runs_status_queuedat_id is missing on analysis_runs");
    }

    Ok(())
}

fn parse_sqlite_version(input: &str) -> anyhow::Result<(u32, u32, u32)> {
    let mut parts = input.split('.');

    let major = parts
        .next()
        .context("missing sqlite major version")?
        .parse::<u32>()
        .context("invalid sqlite major version")?;

    let minor = parts
        .next()
        .context("missing sqlite minor version")?
        .parse::<u32>()
        .context("invalid sqlite minor version")?;

    let patch = parts
        .next()
        .context("missing sqlite patch version")?
        .parse::<u32>()
        .context("invalid sqlite patch version")?;

    Ok((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::parse_sqlite_version;

    #[test]
    fn parses_sqlite_semver_like_string() {
        assert_eq!(parse_sqlite_version("3.43.2").unwrap(), (3, 43, 2));
        assert!(parse_sqlite_version("3.43").is_err());
    }
}
