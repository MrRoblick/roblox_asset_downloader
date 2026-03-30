use std::time::{SystemTime, UNIX_EPOCH};

use async_stream::stream;
use futures_core::Stream;
use tokio::time::{sleep, Duration};

use crate::roblox::models::{
    AssetBatchRequest, AssetBatchResponseItem, AssetDetailsResponse, GameDetail,
    GroupGamesResponse, UserGroupListResponse,
};

pub mod models;

#[derive(Debug, Clone)]
pub struct Endpoints {
    auth: String,
    economy: String,
    games: String,
    asset_delivery: String,
    pub groups: String,
}

impl Endpoints {
    pub fn new(
        auth: String,
        economy: String,
        games: String,
        asset_delivery: String,
        groups: String,
    ) -> Self {
        Self {
            auth,
            economy,
            games,
            asset_delivery,
            groups,
        }
    }

    pub fn default() -> Self {
        Self {
            auth: "auth.roblox.com".into(),
            economy: "economy.roblox.com".into(),
            games: "games.roblox.com".into(),
            asset_delivery: "assetdelivery.roblox.com".into(),
            groups: "groups.roblox.com".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    authorization: String,
    endpoints: Endpoints,
    http_client: reqwest::Client,
    x_csrf_token: String,
    x_csrf_token_time: u64,
}

fn get_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

impl Client {
    pub fn new(authorization: String, endpoints: Endpoints) -> Self {
        Self {
            authorization,
            endpoints,
            http_client: reqwest::Client::builder()
                .cookie_store(false)
                .redirect(reqwest::redirect::Policy::none())
                .gzip(false)
                .brotli(false)
                .deflate(false)
                .build()
                .unwrap(),
            x_csrf_token: "".into(),
            x_csrf_token_time: 0,
        }
    }

    async fn retry_with_backoff<T, F, Fut>(
        &self,
        label: &str,
        attempts: usize,
        mut f: F,
    ) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
    {
        let mut last_err = None;

        for attempt in 1..=attempts {
            match f().await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    eprintln!(
                        "[{}] attempt {}/{} failed: {}",
                        label, attempt, attempts, err
                    );
                    last_err = Some(err);

                    if attempt < attempts {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                }
            }
        }

        Err(last_err.unwrap())
    }

    pub async fn get_csrf(
        &mut self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if get_unix_timestamp() - self.x_csrf_token_time <= 60 && !self.x_csrf_token.is_empty() {
            return Ok(self.x_csrf_token.clone());
        }

        let auth = self.authorization.clone();
        let auth_endpoint = self.endpoints.auth.clone();
        let http_client = self.http_client.clone();

        let token = self
            .retry_with_backoff("get_csrf", 3, || {
                let auth = auth.clone();
                let auth_endpoint = auth_endpoint.clone();
                let http_client = http_client.clone();

                async move {
                    let response = http_client
                        .post(format!("https://{}/v2/logout", auth_endpoint))
                        .header("Cookie", format!(".ROBLOSECURITY={}", auth))
                        .header("Content-Length", "0")
                        .send()
                        .await?;

                    if let Some(token) = response.headers().get("x-csrf-token") {
                        let token_string = token.to_str()?.to_string();
                        return Ok(token_string);
                    }

                    let status = response.status();
                    let body = response.bytes().await.unwrap_or_default();

                    Err(format!(
                        "CSRF token not found in headers (status {}): {}",
                        status,
                        String::from_utf8_lossy(&body)
                    )
                        .into())
                }
            })
            .await?;

        self.x_csrf_token_time = get_unix_timestamp();
        self.x_csrf_token = token.clone();

        Ok(token)
    }

    pub async fn get_asset_details(
        &self,
        asset_id: u64,
    ) -> Result<AssetDetailsResponse, Box<dyn std::error::Error + Send + Sync>> {
        let http_client = self.http_client.clone();
        let economy = self.endpoints.economy.clone();

        self.retry_with_backoff("get_asset_details", 3, || {
            let http_client = http_client.clone();
            let economy = economy.clone();

            async move {
                let response = http_client
                    .get(format!("https://{}/v2/assets/{}/details", economy, asset_id))
                    .send()
                    .await?;

                let status = response.status();
                let bytes = response.bytes().await?;

                if !status.is_success() {
                    return Err(format!(
                        "HTTP status error ({}): {}",
                        status,
                        String::from_utf8_lossy(&bytes)
                    )
                        .into());
                }

                let details = serde_json::from_slice::<AssetDetailsResponse>(&bytes)?;
                Ok(details)
            }
        })
            .await
    }

    pub async fn get_asset_location(
        &mut self,
        asset_id: u64,
        place_id: u64,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let csrf = self.get_csrf().await?;

        let body = vec![AssetBatchRequest {
            asset_id,
            request_id: "0".into(),
        }];

        let url = format!("https://{}/v2/assets/batch", self.endpoints.asset_delivery);
        let http_client = self.http_client.clone();
        let authorization = self.authorization.clone();

        let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;

        for attempt in 1..=3 {
            let response = match http_client
                .post(&url)
                .header("Cookie", format!(".ROBLOSECURITY={}", authorization))
                .header("X-CSRF-TOKEN", &csrf)
                .header("User-Agent", "Roblox/WinInet")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .header("Roblox-Place-Id", place_id.to_string())
                .header("Roblox-Browser-Asset-Request", "false")
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(err) => {
                    eprintln!(
                        "[get_asset_location] attempt {}/3 failed for place {}: {}",
                        attempt, place_id, err
                    );
                    last_err = Some(err.into());

                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(err) => {
                    eprintln!(
                        "[get_asset_location] attempt {}/3 failed reading body for place {}: {}",
                        attempt, place_id, err
                    );
                    last_err = Some(err.into());

                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            if !status.is_success() {
                let body_text = String::from_utf8_lossy(&bytes).to_string();

                let retryable = status.is_server_error()
                    || status.as_u16() == 429;

                if retryable {
                    eprintln!(
                        "[get_asset_location] attempt {}/3 failed for place {}: status {}: {}",
                        attempt, place_id, status, body_text
                    );
                    last_err = Some(format!(
                        "Asset delivery failed (status {}): {}",
                        status, body_text
                    ).into());

                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                } else {
                    return Err(format!(
                        "Asset delivery failed (status {}): {}",
                        status, body_text
                    ).into());
                }
            }

            let items: Vec<AssetBatchResponseItem> = match serde_json::from_slice(&bytes) {
                Ok(items) => items,
                Err(err) => {
                    eprintln!(
                        "[get_asset_location] attempt {}/3 failed parsing JSON for place {}: {}",
                        attempt, place_id, err
                    );
                    last_err = Some(err.into());

                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            let item = match items.into_iter().next() {
                Some(item) => item,
                None => {
                    return Err("Empty response from asset delivery".into());
                }
            };

            let locations = match item.locations {
                Some(locations) => locations,
                None => {
                    return Err("No locations in response".into());
                }
            };

            let location = match locations.into_iter().next() {
                Some(location) => location.location,
                None => {
                    return Err("Locations array is empty".into());
                }
            };

            return Ok(location);
        }

        Err(last_err.unwrap_or_else(|| "Unknown get_asset_location error".into()))
    }

    pub async fn get_user_groups(
        &self,
        user_id: u64,
    ) -> Result<Vec<models::GroupInfo>, Box<dyn std::error::Error + Send + Sync>> {
        let http_client = self.http_client.clone();
        let groups_endpoint = self.endpoints.groups.clone();
        let authorization = self.authorization.clone();

        self.retry_with_backoff("get_user_groups", 3, || {
            let http_client = http_client.clone();
            let groups_endpoint = groups_endpoint.clone();
            let authorization = authorization.clone();

            async move {
                let url = format!(
                    "https://{}/v1/users/{}/groups/roles",
                    groups_endpoint, user_id
                );

                let response = http_client
                    .get(&url)
                    .header("Cookie", format!(".ROBLOSECURITY={}", authorization))
                    .send()
                    .await?;

                let status = response.status();
                let bytes = response.bytes().await?;

                if !status.is_success() {
                    return Err(format!(
                        "Group fetch failed (status {}): {}",
                        status,
                        String::from_utf8_lossy(&bytes)
                    )
                        .into());
                }

                let page: UserGroupListResponse = serde_json::from_slice(&bytes)?;
                Ok(page.data.into_iter().map(|r| r.group).collect())
            }
        })
            .await
    }

    pub fn get_user_games_stream(
        &self,
        user_id: u64,
        limit: u32,
    ) -> impl Stream<Item = Result<GameDetail, Box<dyn std::error::Error + Send + Sync>>> {
        let http_client = self.http_client.clone();
        let games_endpoint = self.endpoints.games.clone();
        let authorization = self.authorization.clone();

        stream! {
            let mut cursor: Option<String> = None;

            loop {
                let mut url = format!(
                    "https://{}/v2/users/{}/games?limit={}",
                    games_endpoint, user_id, limit
                );

                if let Some(ref c) = cursor {
                    url.push_str(&format!("&cursor={}", c));
                }

                let mut page_opt = None;
                let mut last_err = None;

                for attempt in 1..=3 {
                    let result: Result<GroupGamesResponse, Box<dyn std::error::Error + Send + Sync>> = async {
                        let response = http_client
                            .get(&url)
                            .header("Cookie", format!(".ROBLOSECURITY={}", authorization))
                            .send()
                            .await?;

                        let status = response.status();
                        let bytes = response.bytes().await?;

                        if !status.is_success() {
                            return Err(format!(
                                "User games fetch failed (status {}): {}",
                                status,
                                String::from_utf8_lossy(&bytes)
                            ).into());
                        }

                        let page = serde_json::from_slice::<GroupGamesResponse>(&bytes)?;
                        Ok(page)
                    }.await;

                    match result {
                        Ok(page) => {
                            page_opt = Some(page);
                            break;
                        }
                        Err(err) => {
                            eprintln!(
                                "[get_user_games_stream] attempt {}/3 failed for user {}: {}",
                                attempt, user_id, err
                            );
                            last_err = Some(err);

                            if attempt < 3 {
                                sleep(Duration::from_millis(500 * attempt as u64)).await;
                            }
                        }
                    }
                }

                let page = match page_opt {
                    Some(page) => page,
                    None => {
                        yield Err(last_err.unwrap());
                        return;
                    }
                };

                for game in page.data {
                    yield Ok(game);
                }

                match page.next_page_cursor {
                    Some(next) if !next.is_empty() => {
                        cursor = Some(next);
                    }
                    _ => break,
                }
            }
        }
    }

    pub fn get_group_games_stream(
        &self,
        group_id: u64,
        limit: u32,
    ) -> impl Stream<Item = Result<GameDetail, Box<dyn std::error::Error + Send + Sync>>> {
        let http_client = self.http_client.clone();
        let games_endpoint = self.endpoints.games.clone();
        let authorization = self.authorization.clone();

        stream! {
            let mut cursor: Option<String> = None;

            loop {
                let mut url = format!(
                    "https://{}/v2/groups/{}/games?accessFilter=All&sortOrder=Asc&limit={}",
                    games_endpoint, group_id, limit
                );

                if let Some(ref c) = cursor {
                    url.push_str(&format!("&cursor={}", c));
                }

                let mut page_opt = None;
                let mut last_err = None;

                for attempt in 1..=3 {
                    let result: Result<GroupGamesResponse, Box<dyn std::error::Error + Send + Sync>> = async {
                        let response = http_client
                            .get(&url)
                            .header("Cookie", format!(".ROBLOSECURITY={}", authorization))
                            .send()
                            .await?;

                        let status = response.status();
                        let bytes = response.bytes().await?;

                        if !status.is_success() {
                            return Err(format!(
                                "Group games fetch failed (status {}): {}",
                                status,
                                String::from_utf8_lossy(&bytes)
                            ).into());
                        }

                        let page = serde_json::from_slice::<GroupGamesResponse>(&bytes)?;
                        Ok(page)
                    }.await;

                    match result {
                        Ok(page) => {
                            page_opt = Some(page);
                            break;
                        }
                        Err(err) => {
                            eprintln!(
                                "[get_group_games_stream] attempt {}/3 failed for group {}: {}",
                                attempt, group_id, err
                            );
                            last_err = Some(err);

                            if attempt < 3 {
                                sleep(Duration::from_millis(500 * attempt as u64)).await;
                            }
                        }
                    }
                }

                let page = match page_opt {
                    Some(page) => page,
                    None => {
                        yield Err(last_err.unwrap());
                        return;
                    }
                };

                for game in page.data {
                    yield Ok(game);
                }

                match page.next_page_cursor {
                    Some(next) if !next.is_empty() => {
                        cursor = Some(next);
                    }
                    _ => break,
                }
            }
        }
    }
}