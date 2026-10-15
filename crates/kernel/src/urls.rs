use url::Url;

/// Append `path` to `base`, keeping any path segments already present on `base`.
///
/// `Url::join` drops the last segment of a base without trailing slash, which
/// turns `http://host/api` + `v1/chat` into `http://host/v1/chat`. Providers are
/// configured with a server root such as `https://api.groq.com/openai`, so we
/// always treat the base as a directory.
#[must_use]
pub fn join_path(base: &Url, path: &str) -> Url {
    let mut base = base.clone();
    let trimmed = base.path().trim_end_matches('/').to_owned();
    base.set_path(&format!("{trimmed}/"));
    base.set_query(None);
    base.set_fragment(None);
    base.join(path.trim_start_matches('/'))
        .expect("joining a relative path onto an absolute url cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn keeps_existing_base_segments() {
        let joined = join_path(
            &url("https://api.groq.com/openai"),
            "/v1/audio/transcriptions",
        );
        assert_eq!(
            joined.as_str(),
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
    }

    #[test]
    fn handles_trailing_and_leading_slashes() {
        assert_eq!(
            join_path(&url("http://h:1/"), "v1/x").as_str(),
            "http://h:1/v1/x"
        );
        assert_eq!(
            join_path(&url("http://h:1"), "v1/x").as_str(),
            "http://h:1/v1/x"
        );
        assert_eq!(
            join_path(&url("http://h:1/a/"), "/b").as_str(),
            "http://h:1/a/b"
        );
    }

    #[test]
    fn drops_query_of_base() {
        assert_eq!(join_path(&url("http://h/?k=v"), "p").as_str(), "http://h/p");
    }
}
