# 🔒 Roblox Private Asset Downloader

A lightweight and fast API server built with Rust to download private (and public) assets from Roblox.
It supports cookie rotation, proxy routing, and secure token-based authentication to keep your endpoints safe.

## ✨ Features

- 🔑 **Private Asset Access**: Download assets that are normally restricted or hidden.
- 🍪 **Cookie Rotation**: Load multiple cookies from `cookies.txt` or use a single cookie via environment variables.
- 🌐 **Proxy Support**: Rotate requests through a list of proxies using `proxies.txt` to avoid rate-limiting.
- 💾 **Smart Caching**: Assets are cached locally to speed up repeated requests. Auto-clears at 15 GB.
- 🛡️ **Secure API**: Protect your downloader instance with an API token (Header or Query param).
- ⚡ **Blazing Fast**: Written in Rust for maximum performance and low memory footprint.

---

## 🚀 Getting Started

### Prerequisites
- [Rust](https://www.rust-lang.org/tools/install) installed on your system.
- Valid Roblox cookies (`.ROBLOSECURITY`).
- (Optional) A list of HTTP/HTTPS/SOCKS5 proxies.

### Configuration

1. **Cookies**:
   Create a file named `cookies.txt` in the root directory and add your Roblox cookies (one per line).
   *Alternatively, you can use a single cookie via the environment variable (see below).*

2. **Proxies** (Optional):
   Create a file named `proxies.txt` in the root directory and add your proxies (one per line).
   Format example: `http://ip:port`, `socks5://user:pass@ip:port`.

3. **Environment Variables**:
   You need to set an API token to secure your instance. You can also optionally override the `cookies.txt` file by setting the auth cookie directly.

### Build & Run

Export the required environment variables and start the server:

```bash
# Set your Roblox cookie (optional if you have cookies.txt)
export ROBLOX_AUTHORIZATION="your_roblox_cookie" 

# Set your secret API token (required)
export API_TOKEN="your-secret-api-token"

# Build and run the server
cargo run