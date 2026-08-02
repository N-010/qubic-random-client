use random_client::config::AppConfig;

#[tokio::main]
async fn main() -> random_client::AppResult<()> {
    let config = AppConfig::from_cli()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    random_client::run(config).await
}
