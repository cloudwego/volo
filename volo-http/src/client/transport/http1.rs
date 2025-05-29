use hyper::client::conn::http1::Builder;

pub struct Config {
    title_case_headers: bool,
    ignore_invalid_headers_in_responses: bool,
    max_headers: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            title_case_headers: true,
            ignore_invalid_headers_in_responses: false,
            max_headers: None,
        }
    }
}

impl Config {
    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.title_case_headers = title_case_headers;
        self
    }

    /// Set whether HTTP/1 connections will silently ignored malformed header lines.
    ///
    /// If this is enabled and a header line does not start with a valid header
    /// name, or does not include a colon at all, the line will be silently ignored
    /// and no error will be reported.
    ///
    /// Default is false.
    pub fn set_ignore_invalid_headers_in_responses(
        &mut self,
        ignore_invalid_headers_in_responses: bool,
    ) -> &mut Self {
        self.ignore_invalid_headers_in_responses = ignore_invalid_headers_in_responses;
        self
    }

    /// Set the maximum number of headers.
    ///
    /// When a response is received, the parser will reserve a buffer to store headers for optimal
    /// performance.
    ///
    /// If client receives more headers than the buffer size, the error "message header too large"
    /// is returned.
    ///
    /// Note that headers is allocated on the stack by default, which has higher performance. After
    /// setting this value, headers will be allocated in heap memory, that is, heap memory
    /// allocation will occur for each response, and there will be a performance drop of about 5%.
    ///
    /// Default is 100.
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.max_headers = Some(max_headers);
        self
    }
}

pub fn client(config: &Config) -> Builder {
    let mut builder = Builder::new();
    builder
        .title_case_headers(config.title_case_headers)
        .ignore_invalid_headers_in_responses(config.ignore_invalid_headers_in_responses);
    if let Some(max_headers) = config.max_headers {
        builder.max_headers(max_headers);
    }
    builder
}
