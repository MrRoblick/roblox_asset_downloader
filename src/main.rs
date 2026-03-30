use roblox_asset_downloader::roblox::{Client, Endpoints};
use futures_util::StreamExt;

#[tokio::main]
async fn main() {
    let roblox_authorization = std::env::var("ROBLOX_AUTHORIZATION")
        .expect("Expected ROBLOX_AUTHORIZATION in env");

    let mut roblox_client = Client::new(
        roblox_authorization,
        Endpoints::new(
            "auth.roblox.com".into(),
            "economy.roproxy.com".into(),
            "games.roproxy.com".into(),
            "assetdelivery.roblox.com".into(),
        ),
    );

    let result = roblox_client.get_csrf().await.unwrap();
    println!("{}", result);

    let asset_id = 138804671586218u64;
    let details = roblox_client.get_asset_details(asset_id).await.unwrap();

    let games: Vec<_> = {
        let mut stream = std::pin::pin!(
            roblox_client.get_group_games_stream(details.creator.creator_target_id, 100)
        );
        let mut collected = Vec::new();

        while let Some(result) = stream.next().await {
            match result {
                Ok(game) => collected.push(game),
                Err(e) => eprintln!("Error: {}", e),
            }
        }
        collected
    };

    for game in games {
        println!("{:?}", game);
        match roblox_client.get_asset_location(asset_id, game.root_place.id).await {
            Ok(location) => {
                println!("Success Game: {} (id={})", game.name, game.root_place.id);
                println!("   Location: {}", location);
                break;
            }
            Err(e) => {
                eprintln!("Failed {} (place={}): {}", game.name, game.root_place.id, e);
            }
        }
    }
}