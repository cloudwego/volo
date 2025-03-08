use http::Uri;
use volo::FastStr;

pub struct UriExtension(pub Uri);

impl UriExtension {
    pub fn join_path_faststr(&self, path: &FastStr) -> Uri {
        let uri = self.0.clone();
        let original_path = uri.path();

        let joined_path = if original_path.ends_with('/') {
            format!("{}{}", original_path, path.trim_start_matches('/'))
        } else {
            format!("{}/{}", original_path, path.trim_start_matches('/'))
        };

        let p_and_q = match uri.query() {
            Some(query) if !query.is_empty() => format!("{}?{}", joined_path, query),
            _ => joined_path,
        };

        let mut builder = Uri::builder();
        if let Some(schema) = uri.scheme() {
            builder = builder.scheme(schema.as_str());
        }
        if let Some(authority) = uri.authority() {
            builder = builder.authority(authority.as_str())
        }
        builder
            .path_and_query(p_and_q)
            .build()
            .expect("failed to build extended uri")
    }
}

#[cfg(test)]
mod tests {
    use http::Uri;

    use super::UriExtension;

    #[test]
    fn test_join_path_faststr() {
        let base_uri: Uri = "https://tokenizer.agw.prod.internal:8443/override-path?token=1"
            .parse()
            .unwrap();
        let ext = UriExtension(base_uri);
        let computed = ext.join_path_faststr(&"/com.sample.ServiceName/OpenTokenizerSession".into());

        let uri: Uri = "https://tokenizer.agw.prod.internal:8443/override-path/com.sample.ServiceName/OpenTokenizerSession?token=1".parse().unwrap();
        assert_eq!(computed, uri);
    }
}
