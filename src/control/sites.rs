//! Per-host policies (§5 `sites`): the Stage 1 politeness override, the re-crawl
//! interval, and the ladder's starting hint. Rows are operator/CLI-managed; the
//! stage reads them per fetch (the engine never touches the store, §1.2.2).

use rusqlite::Connection;

use super::db::DbError;

/// Where the fetch ladder starts for a host (§5 `fetch_hint`, §8 Stage 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderHint {
    /// Plain HTTP client.
    Plain,
    /// Impersonated browser client (no JS).
    Impersonate,
    /// Full browser rendering (Obscura).
    Browser,
}

impl LadderHint {
    /// The `sites.fetch_hint` discriminator (§5).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LadderHint::Plain => "plain",
            LadderHint::Impersonate => "impersonate",
            LadderHint::Browser => "browser",
        }
    }
}

impl std::str::FromStr for LadderHint {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "plain" => LadderHint::Plain,
            "impersonate" => LadderHint::Impersonate,
            "browser" => LadderHint::Browser,
            _ => return Err(()),
        })
    }
}

/// A `sites` row (§5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePolicy {
    /// Overrides the global politeness floor for this host (ms; §8).
    pub rate_limit_ms: Option<i64>,
    /// Default refresh interval for re-crawl scheduling (§7.5); `None` = never.
    pub recrawl_seconds: Option<i64>,
    /// Ladder starting point; unknown or absent values mean "no hint".
    pub fetch_hint: Option<LadderHint>,
}

/// Reads one host's policy.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn get(conn: &Connection, host: &str) -> Result<Option<SitePolicy>, DbError> {
    let mut stmt = conn
        .prepare("SELECT rate_limit_ms, recrawl_seconds, fetch_hint FROM sites WHERE host = ?1")?;
    let mut rows = stmt.query([host])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let hint: Option<String> = row.get(2)?;
    Ok(Some(SitePolicy {
        rate_limit_ms: row.get(0)?,
        recrawl_seconds: row.get(1)?,
        fetch_hint: hint.and_then(|h| h.parse().ok()),
    }))
}

/// Upserts one host's policy (operator/CLI provisioning; tests).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn set(
    conn: &Connection,
    host: &str,
    rate_limit_ms: Option<i64>,
    recrawl_seconds: Option<i64>,
    fetch_hint: Option<LadderHint>,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO sites (host, rate_limit_ms, recrawl_seconds, fetch_hint)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (host) DO UPDATE SET
             rate_limit_ms = excluded.rate_limit_ms,
             recrawl_seconds = excluded.recrawl_seconds,
             fetch_hint = excluded.fetch_hint",
        rusqlite::params![
            host,
            rate_limit_ms,
            recrawl_seconds,
            fetch_hint.map(LadderHint::as_str)
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use super::super::testing::boot_raw as boot;
    use super::*;

    #[test]
    fn absent_host_has_no_policy() {
        let conn = boot();
        assert_eq!(get(&conn, "example.com").unwrap(), None);
    }

    #[test]
    fn set_then_get_roundtrips_the_policy() {
        let conn = boot();
        set(
            &conn,
            "example.com",
            Some(5_000),
            Some(86_400),
            Some(LadderHint::Browser),
        )
        .unwrap();
        let policy = get(&conn, "example.com").unwrap().expect("row");
        assert_eq!(
            policy,
            SitePolicy {
                rate_limit_ms: Some(5_000),
                recrawl_seconds: Some(86_400),
                fetch_hint: Some(LadderHint::Browser),
            }
        );

        // Unknown hint strings degrade to "no hint" (be-liberal parsing).
        set(&conn, "other.com", None, None, None).unwrap();
        conn.execute(
            "UPDATE sites SET fetch_hint = '??? ' WHERE host = 'other.com'",
            [],
        )
        .unwrap();
        let policy = get(&conn, "other.com").unwrap().expect("row");
        assert_eq!(policy.fetch_hint, None);
    }
}
