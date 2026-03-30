use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use roblox_asset_downloader::roblox;
use roblox_asset_downloader::roblox::{cache, Client, Endpoints};
use roblox_asset_downloader::roblox::proxy::load_proxies;

const CACHE_DIR: &str = "cache";
const MAX_CACHE_BYTES: u64 = 15 * 1024 * 1024 * 1024; // 15 GB
const CACHE_CLEANUP_INTERVAL_SECS: u64 = 60;

struct AppState {
    client: Client,
    asset_location_semaphore: Arc<tokio::sync::Semaphore>,
    cache_dir: PathBuf,
    api_token: String,
}

#[tokio::main]
async fn main() {
    let roblox_authorization =
        std::env::var("ROBLOX_AUTHORIZATION").expect("Expected ROBLOX_AUTHORIZATION in env");

    let api_token = std::env::var("API_TOKEN").expect("Expected API_TOKEN in env");

    let proxies = load_proxies("proxies.txt").await;
    println!("Loaded {} proxies", proxies.len());

    let client = Client::new(
        roblox_authorization,
        Endpoints::new(
            "auth.roblox.com".into(),
            "economy.roblox.com".into(),
            "games.roblox.com".into(),
            "assetdelivery.roblox.com".into(),
            "groups.roblox.com".into(),
        ),
        proxies,
    );

    let csrf = client.get_csrf().await.expect("Failed to get CSRF");
    println!("Initial CSRF: {csrf}");

    let cache_dir = PathBuf::from(CACHE_DIR);
    tokio::fs::create_dir_all(&cache_dir).await.unwrap();

    let state = Arc::new(AppState {
        client,
        asset_location_semaphore: Arc::new(tokio::sync::Semaphore::new(10)),
        cache_dir: cache_dir.clone(),
        api_token,
    });

    tokio::spawn(cache::cache_cleanup_loop(
        cache_dir,
        MAX_CACHE_BYTES,
        CACHE_CLEANUP_INTERVAL_SECS,
    ));

    let app = Router::new()
        .route("/asset/{id}", get(download_asset_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!(
        "Web server listening on http://{}",
        listener.local_addr().unwrap()
    );

    axum::serve(listener, app).await.unwrap();
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let token_from_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    let token_from_query = req
        .uri()
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == "token")
                .map(|(_, value)| value.to_string())
        });

    let provided_token = token_from_header.or(token_from_query);

    match provided_token {
        Some(token) if token == state.api_token => {
            next.run(req).await
        }
        Some(_) => {
            (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"Invalid API token"}"#,
            )
                .into_response()
        }
        None => {
            (
                StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"Missing API token. Use 'Authorization: Bearer <token>' header or '?token=<token>' query parameter"}"#,
            )
                .into_response()
        }
    }
}

async fn download_asset_handler(
    Path(asset_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let cache_path = state.cache_dir.join(asset_id.to_string());

    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        println!("Serving asset {asset_id} from cache");
        return create_file_response(bytes);
    }

    println!("Asset {asset_id} not in cache, downloading...");

    match fetch_and_download_asset(&state, asset_id).await {
        Ok(bytes) => {
            let cache_path_owned = cache_path.clone();
            let bytes_clone = bytes.clone();
            tokio::spawn(async move {
                if let Err(e) = tokio::fs::write(&cache_path_owned, &bytes_clone).await {
                    eprintln!("Failed to write cache for {asset_id}: {e}");
                }
            });

            create_file_response(bytes)
        }
        Err(e) => {
            eprintln!("Error downloading asset {asset_id}: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

fn create_file_response(bytes: Vec<u8>) -> Response {
    let f_format = file_format::FileFormat::from_bytes(&bytes);
    let mime_type = f_format.media_type();
    ([(header::CONTENT_TYPE, mime_type)], bytes).into_response()
}

async fn fetch_and_download_asset(
    state: &AppState,
    asset_id: u64,
) -> Result<Vec<u8>, String> {
    let client = &state.client;

    let details = client
        .get_asset_details(asset_id)
        .await
        .map_err(|e| e.to_string())?;

    let places = collect_places(client, &details).await;

    let mut seen = HashSet::new();
    let places: Vec<(u64, String)> = places
        .into_iter()
        .filter(|(pid, _)| seen.insert(*pid))
        .collect();

    println!("Total unique places to check: {}", places.len());

    let download_url = find_asset_location_parallel(state, asset_id, &places).await;

    let url = download_url
        .ok_or("Could not find a valid place to download this asset")?;

    download_bytes(&state.client, &url).await
}

async fn collect_places(
    client: &Client,
    details: &roblox_asset_downloader::roblox::models::AssetDetailsResponse,
) -> Vec<(u64, String)> {
    use tokio::task::JoinSet;

    if details.creator.creator_type == "Group" {
        let group_id = details.creator.creator_target_id;
        let client = client.clone();

        match client.get_all_group_games(group_id, 100).await {
            Ok(games) => games
                .into_iter()
                .map(|g| (g.root_place.id, format!("group:{group_id}:{}", g.name)))
                .collect(),
            Err(e) => {
                eprintln!("Failed to fetch group games for {group_id}: {e}");
                Vec::new()
            }
        }
    } else {
        let user_id = details.creator.creator_target_id;

        let client_for_games = client.clone();
        let client_for_groups = client.clone();

        let (user_games_result, groups_result) = tokio::join!(
            async move { client_for_games.get_all_user_games(user_id, 50).await },
            async move { client_for_groups.get_user_groups(user_id).await },
        );

        let mut places = Vec::new();

        match user_games_result {
            Ok(games) => {
                for g in games {
                    places.push((g.root_place.id, format!("user:{user_id}:{}", g.name)));
                }
            }
            Err(e) => eprintln!("Failed to fetch user games for {user_id}: {e}"),
        }

        let groups = groups_result.unwrap_or_else(|e| {
            eprintln!("Failed to fetch groups for user {user_id}: {e}");
            Vec::new()
        });

        if !groups.is_empty() {
            let mut join_set = JoinSet::new();

            for group in groups {
                let client = client.clone();
                let group_id = group.id;
                let group_name = group.name.clone();

                join_set.spawn(async move {
                    match client.get_all_group_games(group_id, 100).await {
                        Ok(games) => games
                            .into_iter()
                            .map(|g| {
                                (
                                    g.root_place.id,
                                    format!("group:{group_id}:{group_name}:{}", g.name),
                                )
                            })
                            .collect::<Vec<_>>(),
                        Err(e) => {
                            eprintln!("Failed to fetch group games for {group_id}: {e}");
                            Vec::new()
                        }
                    }
                });
            }

            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok(group_places) => places.extend(group_places),
                    Err(e) => eprintln!("Join error: {e}"),
                }
            }
        }

        places
    }
}

async fn find_asset_location_parallel(
    state: &AppState,
    asset_id: u64,
    places: &[(u64, String)],
) -> Option<String> {
    use tokio::sync::watch;
    use tokio::task::JoinSet;

    if places.is_empty() {
        return None;
    }

    let (found_tx, found_rx) = watch::channel(false);
    let result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let mut join_set = JoinSet::new();

    for (place_id, source) in places.iter().cloned() {
        let client = state.client.clone();
        let semaphore = state.asset_location_semaphore.clone();
        let found_rx = found_rx.clone();
        let result = result.clone();

        join_set.spawn(async move {
            let _permit = match semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };

            if *found_rx.borrow() {
                return;
            }

            match client.get_asset_location(asset_id, place_id).await {
                Ok(location) => {
                    println!("✓ Found via place {place_id} from {source}");
                    let mut res = result.lock().await;
                    if res.is_none() {
                        *res = Some(location);
                    }
                }
                Err(e) => {
                    eprintln!("✗ Denied via place {place_id} from {source}: {e}");
                }
            }
        });
    }

    loop {
        {
            let res = result.lock().await;
            if res.is_some() {
                let _ = found_tx.send(true);
                join_set.abort_all();
                return res.clone();
            }
        }

        match join_set.join_next().await {
            Some(_) => continue,
            None => {
                let res = result.lock().await;
                return res.clone();
            }
        }
    }
}

async fn download_bytes(client: &Client, url: &str) -> Result<Vec<u8>, String> {
    let mut last_err = None;
    let max_attempts = 3u64.max(client.proxy_count() as u64);

    for attempt in 1..=max_attempts {
        let http = client.get_download_client();

        match http.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();

                if status.as_u16() == 429 {
                    let body = resp.bytes().await.unwrap_or_default();
                    let body_text = String::from_utf8_lossy(&body);
                    eprintln!(
                        "Download attempt {attempt}/{max_attempts} got 429: {body_text} (switching proxy)"
                    );
                    client.advance_proxy();
                    last_err = Some(format!("HTTP 429: {body_text}"));
                    if attempt < max_attempts {
                        tokio::time::sleep(std::time::Duration::from_millis(1000 * attempt)).await;
                    }
                    continue;
                }

                if !status.is_success() {
                    let body = resp.bytes().await.unwrap_or_default();
                    let body_text = String::from_utf8_lossy(&body);
                    eprintln!(
                        "Download attempt {attempt}/{max_attempts} failed: HTTP {status}: {body_text}"
                    );
                    last_err = Some(format!("HTTP {status}: {body_text}"));
                    if attempt < max_attempts {
                        tokio::time::sleep(std::time::Duration::from_millis(400 * attempt)).await;
                    }
                    continue;
                }

                match resp.bytes().await {
                    Ok(bytes) => return Ok(bytes.to_vec()),
                    Err(e) => {
                        eprintln!("Download bytes attempt {attempt}/{max_attempts} failed: {e}");
                        last_err = Some(e.to_string());
                    }
                }
            }
            Err(e) => {
                eprintln!("Download request attempt {attempt}/{max_attempts} failed: {e}");
                last_err = Some(e.to_string());
            }
        }
        if attempt < max_attempts {
            tokio::time::sleep(std::time::Duration::from_millis(400 * attempt)).await;
        }
    }

    Err(last_err.unwrap_or_else(|| "Unknown download error".into()))
}