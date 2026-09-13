//! Process entry point for the cleaning stage.

#[tokio::main]
async fn main() -> Result<(), ohara_cleaning::CleaningError> {
    ohara_cleaning::run().await
}
