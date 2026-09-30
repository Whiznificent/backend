#[derive(Debug, PartialEq, Eq)]
pub enum LintViolation {
    DropColumn(String),
    RenameColumn(String),
    NotNullWithoutDefault(String),
    FullTableRewrite(String),
}

pub struct MigrationLinter;

impl MigrationLinter {
    pub fn lint_sql(sql: &str) -> Result<(), Vec<LintViolation>> {
        let mut violations = Vec::new();

        for line in sql.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("--") {
                continue;
            }

            let upper = trimmed.to_uppercase();

            // Check allow override annotation
            if line.contains("-- lint:allow") {
                continue;
            }

            if upper.contains("DROP COLUMN") {
                violations.push(LintViolation::DropColumn(trimmed.to_string()));
            }

            if upper.contains("RENAME COLUMN") {
                violations.push(LintViolation::RenameColumn(trimmed.to_string()));
            }

            if upper.contains("NOT NULL") && !upper.contains("DEFAULT") && upper.contains("ADD COLUMN") {
                violations.push(LintViolation::NotNullWithoutDefault(trimmed.to_string()));
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linter_detects_dangerous_drop_column() {
        let bad_sql = "ALTER TABLE users DROP COLUMN email;";
        let res = MigrationLinter::lint_sql(bad_sql);
        assert!(matches!(res, Err(v) if matches!(v[0], LintViolation::DropColumn(_))));
    }

    #[test]
    fn test_linter_allows_explicitly_annotated_migration() {
        let allowed_sql = "ALTER TABLE users DROP COLUMN email; -- lint:allow reason: deprecation";
        assert!(MigrationLinter::lint_sql(allowed_sql).is_ok());
    }
}
