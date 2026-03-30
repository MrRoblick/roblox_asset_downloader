How to use:

Build & Run:
```bash
export ROBLOX_AUTHORIZATION="your_roblox_cookie" # or make cookies.txt with roblox cookies
export API_TOKEN="your-secret-api-token"
cargo run
```

Http requests:
```bash
# Api Token usage
curl -H "Authorization: Bearer my-secret-token" http://localhost:3000/asset/12345
curl "http://localhost:3000/asset/12345?token=my-secret-token"

# Without token -> 401
curl http://localhost:3000/asset/12345
# {"error":"Missing API token. Use 'Authorization: Bearer <token>' header or '?token=<token>' query parameter"}

# Invalid token -> 403
curl -H "Authorization: Bearer wrong" http://localhost:3000/asset/12345
# {"error":"Invalid API token"}
```
