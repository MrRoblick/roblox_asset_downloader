use std::path::PathBuf;
use tokio::time::{sleep, Duration};

struct CacheEntry {
    path: PathBuf,
    size: u64,
    accessed: std::time::SystemTime,
}

pub async fn cache_cleanup_loop(
    cache_dir: PathBuf,
    max_bytes: u64,
    interval_secs: u64,
) {
    let target_bytes = (max_bytes as f64 * 0.8) as u64;

    println!(
        "Cache cleanup active: max={:.2} GB, target={:.2} GB, interval={interval_secs}s",
        max_bytes as f64 / 1_073_741_824.0,
        target_bytes as f64 / 1_073_741_824.0,
    );

    loop {
        sleep(Duration::from_secs(interval_secs)).await;

        match cleanup_once(&cache_dir, max_bytes, target_bytes).await {
            Ok(Some((removed, freed))) => {
                println!(
                    "Cache cleanup: removed {removed} files, freed {:.2} MB",
                    freed as f64 / 1_048_576.0
                );
            }
            Ok(None) => {
            }
            Err(e) => {
                eprintln!("Cache cleanup error: {e}");
            }
        }
    }
}

async fn cleanup_once(
    cache_dir: &PathBuf,
    max_bytes: u64,
    target_bytes: u64,
) -> Result<Option<(usize, u64)>, Box<dyn std::error::Error + Send + Sync>> {
    let mut entries = Vec::new();
    let mut total_size: u64 = 0;

    let mut dir = tokio::fs::read_dir(cache_dir).await?;

    while let Some(entry) = dir.next_entry().await? {
        let metadata = entry.metadata().await?;

        if !metadata.is_file() {
            continue;
        }

        let size = metadata.len();
        total_size += size;

        let accessed = metadata
            .accessed()
            .or_else(|_| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);

        entries.push(CacheEntry {
            path: entry.path(),
            size,
            accessed,
        });
    }

    if total_size <= max_bytes {
        return Ok(None);
    }

    println!(
        "Cache size {:.2} GB exceeds limit {:.2} GB, cleaning up...",
        total_size as f64 / 1_073_741_824.0,
        max_bytes as f64 / 1_073_741_824.0,
    );

    entries.sort_by(|a, b| a.accessed.cmp(&b.accessed));

    let mut removed_count = 0usize;
    let mut freed_bytes = 0u64;

    for entry in &entries {
        if total_size <= target_bytes {
            break;
        }

        match tokio::fs::remove_file(&entry.path).await {
            Ok(()) => {
                total_size -= entry.size;
                freed_bytes += entry.size;
                removed_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "Failed to remove cache file '{}': {e}",
                    entry.path.display()
                );
            }
        }
    }

    Ok(Some((removed_count, freed_bytes)))
}

#[allow(dead_code)]
pub async fn get_cache_size(cache_dir: &PathBuf) -> Result<u64, std::io::Error> {
    let mut total: u64 = 0;
    let mut dir = tokio::fs::read_dir(cache_dir).await?;

    while let Some(entry) = dir.next_entry().await? {
        let metadata = entry.metadata().await?;
        if metadata.is_file() {
            total += metadata.len();
        }
    }

    Ok(total)
}