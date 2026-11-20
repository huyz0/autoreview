fn decode(
    token: &str,
    key: &jsonwebtoken::DecodingKey,
    validation: &jsonwebtoken::Validation,
) -> jsonwebtoken::errors::Result<jsonwebtoken::TokenData<serde_json::Value>> {
    jsonwebtoken::decode::<serde_json::Value>(token, key, validation)
}

fn inspect_header(token: &str) -> jsonwebtoken::errors::Result<jsonwebtoken::Header> {
    jsonwebtoken::decode_header(token)
}
