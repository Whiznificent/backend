use std::path::Path;
use utoipa::OpenApi;
use zenith_backend::openapi::ApiDoc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = Path::new("openapi.json");
    let generated = ApiDoc::openapi().to_pretty_json()?;
    if std::env::args().any(|arg| arg == "--check") {
        let committed = std::fs::read_to_string(output)?;
        if committed.trim_end() != generated {
            return Err(
                "openapi.json is out of date; run `cargo run --bin generate-openapi`".into(),
            );
        }
        return Ok(());
    }
    std::fs::write(output, format!("{generated}\n"))?;
    Ok(())
}
