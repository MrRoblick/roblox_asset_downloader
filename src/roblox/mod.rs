use std::time::{SystemTime, UNIX_EPOCH};
use async_stream::stream;
use futures_core::Stream;
use crate::roblox::models::{AssetBatchRequest, AssetBatchResponseItem, AssetDetailsResponse, GameDetail, GroupGamesResponse};

pub mod models;

#[derive(Debug, Clone)]
pub struct Endpoints{
    auth: String,
    economy: String,
    games: String,
    asset_delivery: String,
}

impl Endpoints{
    pub fn new(auth: String, economy: String, games: String, asset_delivery: String) -> Self{
        Self{
            auth,
            economy,
            games,
            asset_delivery,
        }
    }
    pub fn default() -> Self{
        Self{
            auth: "auth.roblox.com".into(),
            economy: "economy.roblox.com".into(),
            games: "games.roblox.com".into(),
            asset_delivery: "assetdelivery.roblox.com".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Client{
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

impl Client{
    pub fn new(authorization: String, endpoints: Endpoints) -> Self{
        Self{
            authorization,
            endpoints,
            http_client: reqwest::Client::builder()
                .cookie_store(false)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            x_csrf_token: "".into(),
            x_csrf_token_time: 0,
        }
    }
    pub async fn get_csrf(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        if get_unix_timestamp() - self.x_csrf_token_time > 1800{
            let response = self.http_client
                .post(format!("https://{}/v2/logout", self.endpoints.auth))
                .header("Cookie",format!(".ROBLOSECURITY={}", self.authorization))
                .header("Content-Length", 0)
                .send()
                .await?;
            self.x_csrf_token_time = get_unix_timestamp();

            if let Some(token) = response.headers().get("x-csrf-token") {
                let token_string = token.to_str()?.to_string();
                self.x_csrf_token = token_string.clone();
                return Ok(token_string);
            }
            return Err("CSRF token not found in headers. Check if your Cookie is valid.".into());
        }

        Ok(self.x_csrf_token.clone())
    }
    pub async fn get_asset_details(&self, asset_id: u64) -> Result<AssetDetailsResponse, Box<dyn std::error::Error>>{
        let response = self.http_client.get(format!("https://{}/v2/assets/{}/details", self.endpoints.economy, asset_id)).send().await?;
        let details = response.json::<AssetDetailsResponse>().await?;
        Ok(details)
    }
    pub async fn get_asset_location(
        &mut self,
        asset_id: u64,
        place_id: u64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let csrf = self.get_csrf().await?;

        let body = vec![AssetBatchRequest {
            asset_id,
            request_id: "0".into(),
        }];

        let url = format!(
            "https://{}/v2/assets/batch",
            self.endpoints.asset_delivery
        );

        let body_json = serde_json::to_string(&body)?;
        eprintln!("=== REQUEST ===");
        eprintln!("URL: {}", url);
        eprintln!("Body: {}", body_json);
        eprintln!("CSRF: {}", csrf);
        eprintln!("Place-Id: {}", place_id);

        let response = self
            .http_client
            .post(&url)
            .header("Cookie", format!(".ROBLOSECURITY={}", self.authorization))
            .header("X-CSRF-TOKEN", &csrf)
            .header("User-Agent", "Roblox/WinInet")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Roblox-Place-Id", place_id.to_string())
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let response_text = response.text().await?;
        eprintln!("=== RESPONSE ===");
        eprintln!("Status: {}", status);
        eprintln!("Body: {}", response_text);

        if !status.is_success() {
            return Err(format!(
                "Asset delivery failed (status {}): {}",
                status, response_text
            ).into());
        }

        let items: Vec<AssetBatchResponseItem> = serde_json::from_str(&response_text)?;

        let location = items
            .into_iter()
            .next()
            .ok_or("Empty response from asset delivery")?
            .locations
            .ok_or("No locations in response")?
            .into_iter()
            .next()
            .ok_or("Locations array is empty")?
            .location;

        Ok(location)
    }
    pub fn get_user_games_stream(
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
                    "https://{}/v2/users/{}/games?accessFilter=All&sortOrder=Asc&limit={}",
                    games_endpoint, group_id, limit
                );

                if let Some(ref c) = cursor {
                    url.push_str(&format!("&cursor={}", c));
                }

                let response = http_client
                    .get(&url)
                    .header("Cookie", format!(".ROBLOSECURITY={}", authorization))
                    .send()
                    .await;

                let response = match response {
                    Ok(r) => r,
                    Err(e) => {
                        yield Err(e.into());
                        return;
                    }
                };

                let page: GroupGamesResponse = match response.json().await {
                    Ok(p) => p,
                    Err(e) => {
                        yield Err(e.into());
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

                let response = http_client
                    .get(&url)
                    .header("Cookie", format!(".ROBLOSECURITY={}", authorization))
                    .send()
                    .await;

                let response = match response {
                    Ok(r) => r,
                    Err(e) => {
                        yield Err(e.into());
                        return;
                    }
                };

                let page: GroupGamesResponse = match response.json().await {
                    Ok(p) => p,
                    Err(e) => {
                        yield Err(e.into());
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