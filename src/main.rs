use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;

use roblox_asset_downloader::roblox::{Client, Endpoints};

struct AppState {
    roblox_client: Mutex<Client>,
}



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
            "groups.roproxy.com".into(),
        ),
    );


    let result = roblox_client.get_csrf().await.expect("Failed to get CSRF");
    println!("Initial CSRF: {}", result);

    tokio::fs::create_dir_all("cache").await.unwrap();

    let state = Arc::new(AppState {
        roblox_client: Mutex::new(roblox_client),
    });

    let app = Router::new()
        .route("/asset/{id}", get(download_asset_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Web server listening on http://{}", listener.local_addr().unwrap());

    axum::serve(listener, app).await.unwrap();
}

async fn download_asset_handler(
    Path(asset_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let cache_path = format!("cache/{}", asset_id);

    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        println!("Serving asset {} from cache", asset_id);
        return create_file_response(bytes);
    }

    println!("Asset {} not found in cache, downloading...", asset_id);
    let bytes = match fetch_and_download_asset(&state.roblox_client, asset_id).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error downloading asset {}: {}", asset_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };

    if let Err(e) = tokio::fs::write(&cache_path, &bytes).await {
        eprintln!("Failed to write cache for {}: {}", asset_id, e);
    }

    create_file_response(bytes)
}


fn create_file_response(bytes: Vec<u8>) -> Response {
    let f_format = file_format::FileFormat::from_bytes(&bytes);
    let mime_type = f_format.media_type();

    (
        [(header::CONTENT_TYPE, mime_type)],
        bytes,
    ).into_response()
}

async fn fetch_and_download_asset(
    client_mutex: &Mutex<Client>,
    asset_id: u64,
) -> Result<Vec<u8>, String> {
    let details = {
        let client = client_mutex.lock().await;
        client
            .get_asset_details(asset_id)
            .await
            .map_err(|e| e.to_string())?
    };

    let mut places: Vec<(u64, String)> = Vec::new();

    if details.creator.creator_type == "Group" {
        let group_id = details.creator.creator_target_id;

        let client = client_mutex.lock().await;
        let mut stream = Box::pin(client.get_group_games_stream(group_id, 100));

        while let Some(result) = stream.next().await {
            match result {
                Ok(game) => {
                    places.push((game.root_place.id, format!("group:{}:{}", group_id, game.name)));
                }
                Err(e) => {
                    eprintln!("Failed to fetch group game for group {}: {}", group_id, e);
                }
            }
        }
    } else {
        let user_id = details.creator.creator_target_id;

        {
            let client = client_mutex.lock().await;
            let mut stream = Box::pin(client.get_user_games_stream(user_id, 50));

            while let Some(result) = stream.next().await {
                match result {
                    Ok(game) => {
                        places.push((game.root_place.id, format!("user:{}:{}", user_id, game.name)));
                    }
                    Err(e) => {
                        eprintln!("Failed to fetch user game for user {}: {}", user_id, e);
                    }
                }
            }
        }

        let groups = {
            let client = client_mutex.lock().await;
            match client.get_user_groups(user_id).await {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Failed to fetch groups for user {}: {}", user_id, e);
                    Vec::new()
                }
            }
        };

        for group in groups {
            let group_id = group.id;
            let group_name = group.name.clone();

            let client = client_mutex.lock().await;
            let mut stream = Box::pin(client.get_group_games_stream(group_id, 100));

            while let Some(result) = stream.next().await {
                match result {
                    Ok(game) => {
                        places.push((
                            game.root_place.id,
                            format!("group:{}:{}:{}", group_id, group_name, game.name),
                        ));
                    }
                    Err(e) => {
                        eprintln!(
                            "Failed to fetch group game for user {} in group {}: {}",
                            user_id, group_id, e
                        );
                    }
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    places.retain(|(place_id, _)| seen.insert(*place_id));

    println!("Total places to check (ordered by priority): {}", places.len());

    let mut download_url = None;

    for (place_id, source) in places {
        let mut client = client_mutex.lock().await;

        match client.get_asset_location(asset_id, place_id).await {
            Ok(location) => {
                println!("Success via place {} from {}", place_id, source);
                download_url = Some(location);
                break;
            }
            Err(e) => {
                eprintln!("Denied via place {} from {}: {}", place_id, source, e);
            }
        }
    }

    let url = download_url
        .ok_or("Could not find a valid user/group place to download this asset".to_string())?;

    let mut last_err = None;
    let mut downloaded = None;

    for attempt in 1..=3 {
        match reqwest::get(&url).await {
            Ok(resp) => match resp.bytes().await {
                Ok(bytes) => {
                    downloaded = Some(bytes);
                    break;
                }
                Err(e) => {
                    eprintln!("Download bytes attempt {}/3 failed: {}", attempt, e);
                    last_err = Some(e.to_string());
                }
            },
            Err(e) => {
                eprintln!("Download request attempt {}/3 failed: {}", attempt, e);
                last_err = Some(e.to_string());
            }
        }

        if attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_millis(400 * attempt as u64)).await;
        }
    }

    let bytes = downloaded.ok_or_else(|| {
        last_err.unwrap_or_else(|| "Unknown download error".to_string())
    })?;

    Ok(bytes.to_vec())
}