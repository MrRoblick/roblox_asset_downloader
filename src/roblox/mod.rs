use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

use crate::roblox::cookie::CookieRotator;
use crate::roblox::models::{
    AssetBatchRequest, AssetBatchResponseItem, AssetDetailsResponse, GameDetail,
    GroupGamesResponse, GroupInfo, UserGroupListResponse,
};

pub mod cache;
pub mod cookie;
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
struct CsrfEntry {
    token: String,
    updated_at: u64,
}

#[derive(Debug)]
struct CsrfCache {
    entries: std::collections::HashMap<usize, CsrfEntry>,
}

impl CsrfCache {
    fn new() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    fn get(&self, cookie_idx: usize) -> Option<&str> {
        self.entries.get(&cookie_idx).and_then(|e| {
            if unix_now() - e.updated_at <= 60 {
                Some(e.token.as_str())
            } else {
                None
            }
        })
    }

    fn set(&mut self, cookie_idx: usize, token: String) {
        self.entries.insert(
            cookie_idx,
            CsrfEntry {
                token,
                updated_at: unix_now(),
            },
        );
    }

    fn remove(&mut self, cookie_idx: usize) {
        self.entries.remove(&cookie_idx);
    }
}

#[derive(Debug, Clone)]
struct ProxyPool {
    clients: Vec<(reqwest::Client, String)>,
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

#[derive(Debug)]
struct AuthenticationError {
    message: String,
    cookie_idx: usize,
}

impl std::fmt::Display for AuthenticationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Authentication failed for cookie#{}: {}",
            self.cookie_idx, self.message
        )
    }
}

impl std::error::Error for AuthenticationError {}

impl ProxyPool {
    fn new(proxies: &[String]) -> Self {
        let mut clients = Vec::with_capacity(proxies.len() + 1);

        clients.push((Self::build_http_client(None), "direct".to_string()));

        for proxy_url in proxies {
            match reqwest::Proxy::all(proxy_url) {
                Ok(proxy) => {
                    clients.push((Self::build_http_client(Some(proxy)), proxy_url.clone()));
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
    cookie_rotator: CookieRotator,
    endpoints: Endpoints,
    proxy_pool: ProxyPool,
    csrf_cache: Arc<RwLock<CsrfCache>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

impl Client {
    pub fn new(
        cookie_rotator: CookieRotator,
        endpoints: Endpoints,
        proxies: Vec<String>,
    ) -> Self {
        let proxy_pool = ProxyPool::new(&proxies);

        Self {
            cookie_rotator,
            endpoints,
            proxy_pool,
            csrf_cache: Arc::new(RwLock::new(CsrfCache::new())),
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
    async fn get_csrf_for(
        &self,
        cookie_idx: usize,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        {
            let cache = self.csrf_cache.read().await;
            if let Some(token) = cache.get(cookie_idx) {
                return Ok(token.to_string());
            }
        }

        let mut cache = self.csrf_cache.write().await;

        if let Some(token) = cache.get(cookie_idx) {
            return Ok(token.to_string());
        }

        let http = self.proxy_pool.direct().clone();
        let auth = self.cookie_rotator.get(cookie_idx).await;
        let endpoint = self.endpoints.auth.clone();

        let mut last_err = None;
        for attempt in 1..=3u32 {
            let resp = match http
                .post(format!("https://{endpoint}/v2/logout"))
                .header("Cookie", format!(".ROBLOSECURITY={auth}"))
                .header("Content-Length", "0")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "[get_csrf] cookie#{cookie_idx} attempt {attempt}/3 network error: {e}"
                    );
                    last_err = Some(e.into());
                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            if let Some(tok) = resp.headers().get("x-csrf-token") {
                let token = tok.to_str()?.to_string();
                cache.set(cookie_idx, token.clone());
                return Ok(token);
            }

            let status = resp.status();
            let body = resp.bytes().await.unwrap_or_default();
            let body_text = String::from_utf8_lossy(&body);

            if status.as_u16() == 401 || status.as_u16() == 403 {
                eprintln!(
                    "[get_csrf] cookie#{cookie_idx} got {status}: {body_text} — marking as DEAD"
                );
                drop(cache);
                self.cookie_rotator.mark_dead(cookie_idx).await;
                return Err(Box::new(AuthenticationError {
                    message: format!("HTTP {status}: {body_text}"),
                    cookie_idx,
                }));
            }

            eprintln!(
                "[get_csrf] cookie#{cookie_idx} attempt {attempt}/3: unexpected {status}: {body_text}"
            );
            last_err = Some(
                format!("CSRF token not found ({status}): {body_text}").into(),
            );
            if attempt < 3 {
                sleep(Duration::from_millis(500 * attempt as u64)).await;
            }
        }

        Err(last_err.unwrap())
    }

    pub async fn get_csrf_initial(
        &self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let (idx, _) = self.cookie_rotator.next_alive().await;
        self.get_csrf_for(idx).await
    }

    pub async fn get_asset_details(
        &self,
        asset_id: u64,
    ) -> Result<AssetDetailsResponse, Box<dyn std::error::Error + Send + Sync>> {
        let economy = self.endpoints.economy.clone();

        let mut last_err = None;
        for attempt in 1..=3u32 {
            let http = self.proxy_pool.next().clone();

            let resp = match http
                .get(format!("https://{economy}/v2/assets/{asset_id}/details"))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "[get_asset_details] attempt {attempt}/3 failed: {e}"
                    );
                    last_err = Some(e.into());
                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            let status = resp.status();
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    last_err = Some(e.into());
                    continue;
                }
            };

            if status.as_u16() == 429 {
                let body_text = String::from_utf8_lossy(&bytes).to_string();
                eprintln!(
                    "[get_asset_details] attempt {attempt}/3: 429 rate limited, switching proxy"
                );
                self.proxy_pool.advance();
                last_err = Some(format!("HTTP 429: {body_text}").into());
                if attempt < 3 {
                    sleep(Duration::from_millis(1000 * attempt as u64)).await;
                }
                continue;
            }

            if !status.is_success() {
                let msg = format!(
                    "HTTP {status}: {}",
                    String::from_utf8_lossy(&bytes)
                );
                last_err = Some(msg.into());
                continue;
            }

            return Ok(serde_json::from_slice::<AssetDetailsResponse>(&bytes)?);
        }

        Err(last_err.unwrap())
    }

    pub async fn get_asset_location(
        &self,
        asset_id: u64,
        place_id: u64,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let body = vec![AssetBatchRequest {
            asset_id,
            request_id: "0".into(),
        }];
        let url = format!(
            "https://{}/v1/assets/batch",
            self.endpoints.asset_delivery
        );

        let max_attempts = 3usize.max(self.proxy_pool.len());

        let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;

        for attempt in 1..=max_attempts {
            let (cookie_idx, auth) = self.cookie_rotator.next_alive().await;

            let csrf = match self.get_csrf_for(cookie_idx).await {
                Ok(token) => token,
                Err(e) => {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id}: cookie#{cookie_idx} auth failed, trying next"
                    );
                    last_err = Some(e);
                    continue;
                }
            };

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
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} cookie#{cookie_idx} via [{proxy_label}]: {e}"
                    );
                    last_err = Some(e.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            let status = resp.status();
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!(
                        "[get_asset_location] body read attempt {attempt}/{max_attempts} place {place_id} cookie#{cookie_idx} via [{proxy_label}]: {e}"
                    );
                    last_err = Some(e.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            if !status.is_success() {
                let body_text = String::from_utf8_lossy(&bytes);
                let msg =
                    format!("Asset delivery failed ({status}): {body_text}");

                if status.as_u16() == 429 {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} cookie#{cookie_idx} via [{proxy_label}]: 429 rate limited, switching proxy only"
                    );
                    self.proxy_pool.advance();
                    last_err = Some(msg.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(1000 * attempt as u64))
                            .await;
                    }
                    continue;
                }

                if status.as_u16() == 401 || status.as_u16() == 403 {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} cookie#{cookie_idx} via [{proxy_label}]: {status} auth error, marking cookie dead"
                    );
                    self.cookie_rotator.mark_dead(cookie_idx).await;
                    {
                        let mut cache = self.csrf_cache.write().await;
                        cache.remove(cookie_idx);
                    }
                    last_err = Some(msg.into());
                    continue;
                }

                if status.is_server_error() {
                    eprintln!(
                        "[get_asset_location] attempt {attempt}/{max_attempts} place {place_id} cookie#{cookie_idx} via [{proxy_label}]: {msg}"
                    );
                    last_err = Some(msg.into());
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(500 * attempt as u64))
                            .await;
                    }
                    continue;
                }

                return Err(msg.into());
            }

            let items: Vec<AssetBatchResponseItem> = serde_json::from_slice(&bytes)?;
            let item = items.into_iter().next().ok_or("Empty response array")?;

            let download_url = item.location.ok_or_else(|| {
                format!("No location provided in response (Archived: {})", item.is_archived)
            })?;

            println!(
                "[get_asset_location] ✓ place {place_id} resolved via [{proxy_label}] cookie#{cookie_idx}"
            );
            return Ok(download_url);
        }

        Err(last_err
            .unwrap_or_else(|| "Unknown get_asset_location error".into()))
    }

    pub async fn get_user_groups(
        &self,
        user_id: u64,
    ) -> Result<Vec<GroupInfo>, Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = self.endpoints.groups.clone();

        let mut last_err = None;
        for attempt in 1..=3u32 {
            let http = self.proxy_pool.next().clone();
            let (_, auth) = self.cookie_rotator.next_alive().await;

            let resp = match http
                .get(format!(
                    "https://{endpoint}/v1/users/{user_id}/groups/roles"
                ))
                .header("Cookie", format!(".ROBLOSECURITY={auth}"))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e.into());
                    if attempt < 3 {
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                    continue;
                }
            };

            let status = resp.status();
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    last_err = Some(e.into());
                    continue;
                }
            };

            if status.as_u16() == 429 {
                self.proxy_pool.advance();
                let body_text = String::from_utf8_lossy(&bytes).to_string();
                last_err =
                    Some(format!("HTTP 429: {body_text}").into());
                if attempt < 3 {
                    sleep(Duration::from_millis(1000 * attempt as u64)).await;
                }
                continue;
            }

            if !status.is_success() {
                last_err = Some(
                    format!(
                        "Group fetch failed ({status}): {}",
                        String::from_utf8_lossy(&bytes)
                    )
                        .into(),
                );
                continue;
            }

            let page: UserGroupListResponse =
                serde_json::from_slice(&bytes)?;
            return Ok(page.data.into_iter().map(|r| r.group).collect());
        }

        Err(last_err.unwrap())
    }

    pub async fn get_all_user_games(
        &self,
        user_id: u64,
        page_limit: u32,
    ) -> Result<Vec<GameDetail>, Box<dyn std::error::Error + Send + Sync>>
    {
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
    ) -> Result<Vec<GameDetail>, Box<dyn std::error::Error + Send + Sync>>
    {
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
    ) -> Result<Vec<GameDetail>, Box<dyn std::error::Error + Send + Sync>>
    {
        let mut all_games = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let separator =
                if base_url.contains('?') { "&" } else { "?" };
            let mut url =
                format!("{base_url}{separator}limit={page_limit}");
            if let Some(ref c) = cursor {
                url.push_str(&format!("&cursor={c}"));
            }

            let mut last_err = None;
            let mut page_result = None;

            for attempt in 1..=3u32 {
                let http = self.proxy_pool.next().clone();
                let (_, auth) = self.cookie_rotator.next_alive().await;

                let resp = match http
                    .get(&url)
                    .header("Cookie", format!(".ROBLOSECURITY={auth}"))
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!(
                            "[{label}] attempt {attempt}/3 failed: {e}"
                        );
                        last_err = Some(e.into());
                        if attempt < 3 {
                            sleep(Duration::from_millis(
                                500 * attempt as u64,
                            ))
                                .await;
                        }
                        continue;
                    }
                };

                let status = resp.status();
                let bytes = match resp.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        last_err = Some(e.into());
                        continue;
                    }
                };

                if status.as_u16() == 429 {
                    self.proxy_pool.advance();
                    let body_text =
                        String::from_utf8_lossy(&bytes).to_string();
                    eprintln!(
                        "[{label}] attempt {attempt}/3: 429, switching proxy"
                    );
                    last_err = Some(
                        format!("HTTP 429: {body_text}").into(),
                    );
                    if attempt < 3 {
                        sleep(Duration::from_millis(
                            1000 * attempt as u64,
                        ))
                            .await;
                    }
                    continue;
                }

                if !status.is_success() {
                    last_err = Some(
                        format!(
                            "Games fetch failed ({status}): {}",
                            String::from_utf8_lossy(&bytes)
                        )
                            .into(),
                    );
                    continue;
                }

                page_result = Some(
                    serde_json::from_slice::<GroupGamesResponse>(&bytes)?,
                );
                break;
            }

            let page = match page_result {
                Some(p) => p,
                None => return Err(last_err.unwrap()),
            };

            all_games.extend(page.data);

            match page.next_page_cursor {
                Some(next) if !next.is_empty() => {
                    cursor = Some(next)
                }
                _ => break,
            }
        }

        Ok(all_games)
    }
}