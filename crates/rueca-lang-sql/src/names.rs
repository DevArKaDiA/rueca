//! Identifier normalization (crate docs, *Names*): unquoted identifiers
//! fold to lower case in every dialect; quoted ones keep their case where
//! the dialect compares them case-sensitively (PostgreSQL, Snowflake,
//! Redshift, DuckDB, ANSI) and fold where it does not (MySQL, SQLite, T-SQL,
//! BigQuery); a quoted Snowflake identifier in upper case equals the
//! unquoted one. A leading default schema is dropped (`public.`, `dbo.`,
//! `main.`).

use crate::dialect::Dialect;

/// One identifier as written: its text without quotes and whether it was
/// quoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub quoted: bool,
}

/// The normalized form of one identifier.
pub fn fold(dialect: Dialect, part: &Part) -> String {
    if !part.quoted || dialect.quoted_case_insensitive() {
        return part.text.to_lowercase();
    }
    if dialect == Dialect::Snowflake && part.text == part.text.to_uppercase() {
        return part.text.to_lowercase();
    }
    part.text.clone()
}

/// Normalized segments of a qualified name, the default schema dropped
/// when it leads a name of two or more segments.
pub fn segments(dialect: Dialect, parts: &[Part]) -> Vec<String> {
    let mut out: Vec<String> = parts.iter().map(|p| fold(dialect, p)).collect();
    if out.len() >= 2
        && let Some(schema) = dialect.default_schema()
        && out[out.len() - 2] == schema
    {
        out.remove(out.len() - 2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str, q: bool) -> Part {
        Part { text: t.into(), quoted: q }
    }

    #[test]
    fn folding_per_dialect() {
        assert_eq!(fold(Dialect::Postgres, &p("Users", false)), "users");
        assert_eq!(fold(Dialect::Postgres, &p("Users", true)), "Users");
        assert_eq!(fold(Dialect::MySql, &p("Users", true)), "users");
        assert_eq!(fold(Dialect::MsSql, &p("Users", true)), "users");
        assert_eq!(fold(Dialect::Snowflake, &p("USERS", true)), "users");
        assert_eq!(fold(Dialect::Snowflake, &p("Users", true)), "Users");
    }

    #[test]
    fn default_schemas_dropped() {
        let n = [p("public", false), p("users", false)];
        assert_eq!(segments(Dialect::Postgres, &n), ["users"]);
        assert_eq!(segments(Dialect::MySql, &n), ["public", "users"]);
        let n = [p("dbo", true), p("Orders", true)];
        assert_eq!(segments(Dialect::MsSql, &n), ["orders"]);
        let n = [p("db", false), p("dbo", false), p("t", false)];
        assert_eq!(segments(Dialect::MsSql, &n), ["db", "t"]);
    }
}
