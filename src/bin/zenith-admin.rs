use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("fixtures") if arguments.get(1).map(String::as_str) == Some("generate") => {
            let wallets = value(&arguments, "--wallets")?.parse()?;
            let days = value(&arguments, "--days")?.parse()?;
            let seed = value(&arguments, "--seed")?.parse()?;
            let database_url = optional_value(&arguments, "--database-url")
                .unwrap_or_else(|| "sqlite://zenith-fixtures.db".to_string());
            if !database_url.starts_with("sqlite://") {
                return Err("fixtures require a sqlite:// database URL".into());
            }
            let db = zenith_backend::db::init_pool(&database_url).await;
            let summary = zenith_backend::admin::generate_fixtures(db, wallets, days, seed).await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Some("snapshot") if arguments.get(1).map(String::as_str) == Some("anonymize") => {
            let from = value(&arguments, "--from")?;
            let to = value(&arguments, "--to")?;
            zenith_backend::admin::anonymize_snapshot(from, to).await?;
            println!("Created anonymized snapshot at {to}");
        }
        _ => {
            return Err(
                "usage: zenith-admin fixtures generate --wallets N --days D --seed S [--database-url sqlite://...]\n       zenith-admin snapshot anonymize --from sqlite://... --to sqlite://..."
                    .into(),
            );
        }
    }
    Ok(())
}

fn value<'a>(arguments: &'a [String], flag: &str) -> Result<&'a str, Box<dyn Error>> {
    let index = arguments
        .iter()
        .position(|argument| argument == flag)
        .ok_or_else(|| format!("missing required {flag}"))?;
    arguments
        .get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("missing value for {flag}").into())
}

fn optional_value(arguments: &[String], flag: &str) -> Option<String> {
    let index = arguments.iter().position(|argument| argument == flag)?;
    arguments.get(index + 1).cloned()
}
