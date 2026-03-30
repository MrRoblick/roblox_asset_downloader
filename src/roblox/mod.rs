use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

use crate::roblox::models::{
    AssetBatchRequest, AssetBatchResponseItem, AssetDetailsResponse, GameDetail,
    GroupGamesResponse, GroupInfo, UserGroupListResponse,
};

pub mod cache;
pub mod models;
pub mod proxy;

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

#[derive(Debug)]
struct CsrfState {
    token: String,
    updated_at: u64,
}

#[derive(Debug, Clone)]
struct ProxyPool {
    clients: Vec<(reqwest::Client, String)>, // (client, label)
    counter: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct RateLimitedError {
    message: String,
}

impl std::fmt::Display for RateLimitedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rate limited: {}", self.message)
    }
}

impl std::error::Error for RateLimitedError {}

impl ProxyPool {
    fn new(proxies: &[String]) -> Self {
        let mut clients = Vec::with_capacity(proxies.len() + 1);

        clients.push((Self::build_http_client(None), "direct".to_string()));

        for proxy_url in proxies {
            match reqwest::Proxy::all(proxy_url) {
                Ok(proxy) => {
                    clients.push((
                        Self::build_http_client(Some(proxy)),
                        proxy_url.clone(),
                    ));
                    println!("  ✓ Proxy added: {proxy_url}");
                }
                Err(e) => {
                    eprintln!("  ✗ Invalid proxy '{proxy_url}': {e}");
                }
            }
        }

        Self {
            clients,
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn build_http_client(proxy: Option<reqwest::Proxy>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .cookie_store(false)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .gzip(true)
            .brotli(true)
            .deflate(true);

        if let Some(p) = proxy {
            builder = builder.proxy(p);
        }

        builder.build().unwrap()
    }

    fn direct(&self) -> &reqwest::Client {
        &self.clients[0].0
    }

    fn next(&self) -> &reqwest::Client {
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        &self.clients[idx].0
    }

    fn next_with_index(&self) -> (usize, &reqwest::Client) {
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        (idx, &self.clients[idx].0)
    }

    fn get_by_index(&self, idx: usize) -> &reqwest::Client {
        &self.clients[idx % self.clients.len()].0
    }

    fn label(&self, idx: usize) -> &str {
        &self.clients[idx % self.clients.len()].1
    }

    fn advance(&self) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }

    fn len(&self) -> usize {
        self.clients.len()
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    authorization: String,
    endpoints: Endpoints,
    proxy_pool: ProxyPool,
    csrf: Arc<RwLock<CsrfState>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

async fn retry<T, F, Fut>(
    label: &str,
    attempts: usize,
    proxy_pool: &ProxyPool,
    mut f: F,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
{
    let mut last_err = None;
    for attempt in 1..=attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let is_rate_limited = e.downcast_ref::<RateLimitedError>().is_some();

                eprintln!("[{label}] attempt {attempt}/{attempts} failed: {e}");

                if is_rate_limited {
                    proxy_pool.advance();
                    eprintln!("[{label}] switched to next proxy due to rate limiting");
                }

                last_err = Some(e);
                if attempt < attempts {
                    let delay = if is_rate_limited {
                        Duration::from_millis(1000 * attempt as u64)
                    } else {
                        Duration::from_millis(500 * attempt as u64)
                    };
                    sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap())
}

impl Client {
    pub fn new(authorization: String, endpoints: Endpoints, proxies: Vec<String>) -> Self {
        let proxy_pool = ProxyPool::new(&proxies);

        println!(
            "Client initialized: {} total HTTP clients ({} proxies + direct)",
            proxy_pool.clients.len(),
            proxy_pool.clients.len() - 1,
        );

        Self {
            authorization,
            endpoints,
            proxy_pool,
            csrf: Arc::new(RwLock::new(CsrfState {
                token: String::new(),
                updated_at: 0,
            })),
        }
    }

    pub fn get_download_client(&self) -> reqwest::Client {
        self.proxy_pool.next().clone()
    }

    pub fn advance_proxy(&self) {
        self.proxy_pool.advance();
    }

    pub fn proxy_count(&self) -> usize {
        self.proxy_pool.len()
    }

    pub async fn get_csrf(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        {
            let state = self.csrf.read().await;
            if !state.token.is_empty() && unix_now() - state.updated_at <= 60 {
                return Ok(state.token.clone());
            }
        }

        let mut state = self.csrf.write().await;

        if !state.token.is_empty() && unix_now() - state.updated_at <= 60 {
            return Ok(state.token.clone());
        }

        let http = self.proxy_pool.direct().clone();
        let auth = self.authorization.clone();
        let endpoint = self.endpoints.auth.clone();
        let proxy_pool = self.proxy_pool.clone();

        let token = retry("get_csrf", 3, &proxy_pool, || {
            let http = http.clone();
            let auth = auth.clone();
            let endpoint = endpoint.clone();
            async move {
                let resp = http
                    .post(format!("https://{endpoint}/v2/logout"))
                    .header("Cookie", format!(".ROBLOSECURITY={auth}"))
                    .header("Content-Length", "0")
                    .send()
                    .await?;

                if let Some(tok) = resp.headers().get("x-csrf-token") {
                    return Ok(tok.to_str()?.to_string());
                }

                let status = resp.status();
                let body = resp.bytes().await.unwrap_or_default();
                Err(format!(
                    "CSRF token not found ({status}): {}",
                    String::from_utf8_lossy(&body)
                )
                    .into())
            }
        })
            .await?;

        state.token = token.clone();
        state.updated_at = unix_now();
        Ok(token)
    }

    pub async fn get_asset_details(
        &self,
        asset_id: u64,
    ) -> Result<AssetDetailsResponse, Box<dyn std::error::Error + Send + Sync>> {
        let economy = self.endpoints.economy.clone();
        let proxy_pool = self.proxy_pool.clone();

        retry("get_asset_details", 3, &proxy_pool, || {
            let http = self.proxy_pool.next().clone();
            let economy = economy.clone();
            async move {
                let resp = http
                    .get(format!("https://{economy}/v2/assets/{asset_id}/details"))
                    .send()
                    .await?;

                let status = resp.status();
                let bytes = resp.bytes().await?;

                if status.as_u16() == 429 {
                    let body_text = String::from_utf8_lossy(&bytes).to_string();
                    return Err(Box::new(RateLimitedError {
                        message: format!("HTTP {status}: {body_text}"),
                    })
                        as Box<dyn std::error::Error + Send + Sync>);
                }

                if !status.is_success() {
                    return Err(format!(
                        "HTTP {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    )
                        .into());
                }

                Ok(serde_json::from_slice::<AssetDetailsResponse>(&bytes)?)
            }
        })
            .await
    }

    pub async fn get_asset_location(
        &self,
        asset_id: u64,
        place_id: u64,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let csrf = self.get_csrf().await?;
        let body = vec![AssetBatchRequest {
            asset_id,
            request_id: "0".into(),
        }];
        let url = format!(
            "https://{}/v2/assets/batch",
            self.endpoints.asset_delivery
        );
        let auth = self.authorization.clone();

        let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;

        let max_attempts = 3u64.max(self.proxy_pool.len() as u64);

        for attempt in 1..=max_attempts {
            let (proxy_idx, http) = self.proxy_pool.next_with_index();
            let http = http.clone();
            let proxy_label = self.proxy_pool.label(proxy_idx).to_string();

            let resp = match http
                .post(&url)
                .header("Cookie", format!(".ROBLOSECURITY={auth}"))
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
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} via [{proxy_label}]: {e}"
                    );
                    last_err = Some(e.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(500 * attempt)).await;
                    }
                    continue;
                }
            };

            let status = resp.status();
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!(
                        "[get_asset_location] body read attempt {attempt}/{max_attempts} place {place_id} via [{proxy_label}]: {e}"
                    );
                    last_err = Some(e.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(500 * attempt)).await;
                    }
                    continue;
                }
            };

            if !status.is_success() {
                let body_text = String::from_utf8_lossy(&bytes);
                let msg = format!("Asset delivery failed ({status}): {body_text}");

                if status.as_u16() == 429 {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} via [{proxy_label}]: 429 rate limited, switching proxy"
                    );
                    last_err = Some(msg.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(1000 * attempt)).await;
                    }
                    continue;
                }

                if status.is_server_error() {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} via [{proxy_label}]: {msg}"
                    );
                    last_err = Some(msg.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(500 * attempt)).await;
                    }
                    continue;
                }

                return Err(msg.into());
            }

            let items: Vec<AssetBatchResponseItem> = serde_json::from_slice(&bytes)?;
            let item = items.into_iter().next().ok_or("Empty response")?;
            let locations = item.locations.ok_or("No locations in response")?;
            let loc = locations.into_iter().next().ok_or("Locations array empty")?;

            println!(
                "[get_asset_location] ✓ place {place_id} resolved via [{proxy_label}]"
            );
            return Ok(loc.location);
        }

        Err(last_err.unwrap_or_else(|| "Unknown get_asset_location error".into()))
    }

    pub async fn get_user_groups(
        &self,
        user_id: u64,
    ) -> Result<Vec<GroupInfo>, Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = self.endpoints.groups.clone();
        let auth = self.authorization.clone();
        let proxy_pool = self.proxy_pool.clone();

        retry("get_user_groups", 3, &proxy_pool, || {
            let http = self.proxy_pool.next().clone();
            let endpoint = endpoint.clone();
            let auth = auth.clone();
            async move {
                let resp = http
                    .get(format!(
                        "https://{endpoint}/v1/users/{user_id}/groups/roles"
                    ))
                    .header("Cookie", format!(".ROBLOSECURITY={auth}"))
                    .send()
                    .await?;

                let status = resp.status();
                let bytes = resp.bytes().await?;

                if status.as_u16() == 429 {
                    let body_text = String::from_utf8_lossy(&bytes).to_string();
                    return Err(Box::new(RateLimitedError {
                        message: format!("HTTP {status}: {body_text}"),
                    })
                        as Box<dyn std::error::Error + Send + Sync>);
                }

                if !status.is_success() {
                    return Err(format!(
                        "Group fetch failed ({status}): {}",
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

    pub async fn get_all_user_games(
        &self,
        user_id: u64,
        page_limit: u32,
    ) -> Result<Vec<GameDetail>, Box<dyn std::error::Error + Send + Sync>> {
        self.fetch_all_games_paginated(
            &format!(
                "https://{}/v2/users/{user_id}/games",
                self.endpoints.games
            ),
            page_limit,
            "get_all_user_games",
        )
            .await
    }

    pub async fn get_all_group_games(
        &self,
        group_id: u64,
        page_limit: u32,
    ) -> Result<Vec<GameDetail>, Box<dyn std::error::Error + Send + Sync>> {
        self.fetch_all_games_paginated(
            &format!(
                "https://{}/v2/groups/{group_id}/games?accessFilter=All&sortOrder=Asc",
                self.endpoints.games
            ),
            page_limit,
            "get_all_group_games",
        )
            .await
    }

    async fn fetch_all_games_paginated(
        &self,
        base_url: &str,
        page_limit: u32,
        label: &str,
    ) -> Result<Vec<GameDetail>, Box<dyn std::error::Error + Send + Sync>> {
        let auth = self.authorization.clone();
        let mut all_games = Vec::new();
        let mut cursor: Option<String> = None;
        let proxy_pool = self.proxy_pool.clone();

        loop {
            let separator = if base_url.contains('?') { "&" } else { "?" };
            let mut url = format!("{base_url}{separator}limit={page_limit}");
            if let Some(ref c) = cursor {
                url.push_str(&format!("&cursor={c}"));
            }

            let auth = auth.clone();
            let label_owned = label.to_string();

            let page: GroupGamesResponse = retry(&label_owned, 3, &proxy_pool, || {
                let http = self.proxy_pool.next().clone();
                let auth = auth.clone();
                let url = url.clone();
                async move {
                    let resp = http
                        .get(&url)
                        .header("Cookie", format!(".ROBLOSECURITY={auth}"))
                        .send()
                        .await?;

                    let status = resp.status();
                    let bytes = resp.bytes().await?;

                    if status.as_u16() == 429 {
                        let body_text = String::from_utf8_lossy(&bytes).to_string();
                        return Err(Box::new(RateLimitedError {
                            message: format!("HTTP {status}: {body_text}"),
                        })
                            as Box<dyn std::error::Error + Send + Sync>);
                    }

                    if !status.is_success() {
                        return Err(format!(
                            "Games fetch failed ({status}): {}",
                            String::from_utf8_lossy(&bytes)
                        )
                            .into());
                    }
                    Ok(serde_json::from_slice(&bytes)?)
                }
            })
                .await?;

            all_games.extend(page.data);

            match page.next_page_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }

        Ok(all_games)
    }
}