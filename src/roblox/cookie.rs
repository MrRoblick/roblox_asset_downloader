use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug)]
struct CookieEntry {
    value: String,
    is_dead: bool,
}

#[derive(Debug, Clone)]
pub struct CookieRotator {
    cookies: Arc<RwLock<Vec<CookieEntry>>>,
    counter: Arc<AtomicUsize>,
}

impl CookieRotator {
    pub fn new(cookies: Vec<String>) -> Self {
        if cookies.is_empty() {
            panic!("Cookie list is empty! Add at least one cookie to cookies.txt");
        }
        println!("CookieRotator initialized with {} cookies", cookies.len());
        let entries = cookies
            .into_iter()
            .map(|value| CookieEntry {
                value,
                is_dead: false,
            })
            .collect();
        Self {
            cookies: Arc::new(RwLock::new(entries)),
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn next_alive(&self) -> (usize, String) {
        let cookies = self.cookies.read().await;
        let len = cookies.len();
        let start = self.counter.fetch_add(1, Ordering::Relaxed);

        for offset in 0..len {
            let idx = (start + offset) % len;
            if !cookies[idx].is_dead {
                return (idx, cookies[idx].value.clone());
            }
        }

        let idx = start % len;
        eprintln!(
            "WARNING: All cookies are marked as dead! Using cookie#{idx} anyway"
        );
        (idx, cookies[idx].value.clone())
    }

    pub async fn get(&self, idx: usize) -> String {
        let cookies = self.cookies.read().await;
        cookies[idx % cookies.len()].value.clone()
    }

    pub async fn mark_dead(&self, idx: usize) {
        let mut cookies = self.cookies.write().await;
        let len = cookies.len();
        let idx = idx % len;
        if !cookies[idx].is_dead {
            cookies[idx].is_dead = true;
            let alive_count = cookies.iter().filter(|c| !c.is_dead).count();
            eprintln!(
                "Cookie#{idx} marked as DEAD. Alive cookies remaining: {alive_count}/{}",
                len
            );
        }
    }

    pub async fn is_alive(&self, idx: usize) -> bool {
        let cookies = self.cookies.read().await;
        !cookies[idx % cookies.len()].is_dead
    }

    pub async fn len(&self) -> usize {
        self.cookies.read().await.len()
    }

    pub async fn alive_count(&self) -> usize {
        self.cookies.read().await.iter().filter(|c| !c.is_dead).count()
    }
}

pub async fn load_cookies<P: AsRef<Path>>(path: P) -> Vec<String> {
    let path = path.as_ref();

    if !path.exists() {
        eprintln!("Cookie file '{}' not found", path.display());
        return Vec::new();
    }

    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let cookies: Vec<String> = content
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();

            if cookies.is_empty() {
                eprintln!("Cookie file '{}' is empty", path.display());
            } else {
                println!(
                    "Loaded {} cookies from '{}'",
                    cookies.len(),
                    path.display()
                );
            }

            cookies
        }
        Err(e) => {
            eprintln!("Failed to read cookie file '{}': {e}", path.display());
            Vec::new()
        }
    }
}