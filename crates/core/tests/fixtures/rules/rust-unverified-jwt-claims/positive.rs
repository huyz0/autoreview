fn inspect(token: &str) -> jsonwebtoken::errors::Result<jsonwebtoken::TokenData<serde_json::Value>> {
    jsonwebtoken::dangerous::insecure_decode::<serde_json::Value>(token)
}
