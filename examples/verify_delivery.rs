mod pb {
    tonic::include_proto!("lightnode");
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = std::env::args().nth(1).ok_or("targets.tsv required")?;
    let client = pb::light_node_client::LightNodeClient::connect("http://127.0.0.1:50052").await?;
    let text = std::fs::read_to_string(path)?;
    let mut tasks = tokio::task::JoinSet::new();
    for line in text.lines() {
        let (tick, hex) = line.split_once('\t').ok_or("invalid target")?;
        let tick: u32 = tick.parse()?;
        let digest: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
            .collect::<Result<_, _>>()?;
        if digest.len() != 32 {
            return Err("invalid digest".into());
        }
        let mut client = client.clone();
        tasks.spawn(async move {
            for _ in 0..3 {
                let result = tokio::time::timeout(std::time::Duration::from_secs(8),client.get_tick_transactions(pb::GetTickTransactionsRequest { tick })).await;
                if let Ok(Ok(response)) = result {
                    let d=response.into_inner();
                    if d.ok && d.tick==tick && d.digests_complete {
                        let state=if d.transaction_digests.is_empty() {"empty"} else if d.transaction_digests.contains(&digest) {"included"} else {"missing"};
                        return serde_json::json!({"tick":tick,"state":state,"count":d.transaction_digests.len()});
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            serde_json::json!({"tick":tick,"state":"unavailable"})
        });
        if tasks.len() >= 4 {
            println!("{}", tasks.join_next().await.unwrap()?);
        }
    }
    while let Some(result) = tasks.join_next().await {
        println!("{}", result?);
    }
    Ok(())
}
