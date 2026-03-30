use std::path::Path;

pub async fn load_proxies<P: AsRef<Path>>(path: P) -> Vec<String> {
    let path = path.as_ref();

    if !path.exists() {
        eprintln!(
            "Proxy file '{}' not found, running without proxies",
            path.display()
        );
        return Vec::new();
    }

    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let proxies: Vec<String> = content
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();

            if proxies.is_empty() {
                eprintln!("Proxy file is empty, running without proxies");
            }

            proxies
        }
        Err(e) => {
            eprintln!("Failed to read proxy file '{}': {e}", path.display());
            Vec::new()
        }
    }
}