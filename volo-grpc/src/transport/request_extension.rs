use http::{uri::Parts, Uri};
use volo::{net::Address, FastStr};

#[derive(Clone)]
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

        uri_to_builder(uri)
            .path_and_query(p_and_q)
            .build()
            .expect("failed to build extended uri")
    }

    pub fn base_url(&self) -> Uri {
        let uri = self.0.clone();
        uri_to_builder(uri)
            .path_and_query("/")
            .build()
            .expect("failed to build base uri")
    }
}

pub fn uri_to_builder(uri: Uri) -> http::uri::Builder {
    let parts: Parts = uri.into_parts();
    let mut builder = Uri::builder();
    if let Some(scheme) = parts.scheme {
        builder = builder.scheme(scheme);
    }
    if let Some(authority) = parts.authority {
        builder = builder.authority(authority);
    }
    if let Some(path_and_query) = parts.path_and_query {
        builder = builder.path_and_query(path_and_query);
    }
    builder
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
        let computed =
            ext.join_path_faststr(&"/com.sample.ServiceName/OpenTokenizerSession".into());

        let uri: Uri = "https://1.1.1.1:8443/override-path/com.sample.ServiceName/OpenTokenizerSession?token=1".parse().unwrap();
        assert_eq!(computed, uri);
    }
}
