use std::{net::SocketAddr, time::Duration};

use http::StatusCode;
use tempfile::NamedTempFile;
use tokio::{io::AsyncWriteExt, net::TcpListener};
use volo_http::{
    body::BodyConversion,
    client::multipart::{Form, Part},
    error::BoxError,
    server::{
        Router, Server,
        route::post,
        utils::multipart::{Multipart, MultipartRejectionError},
    },
};

async fn upload(mut multipart: Multipart) -> Result<String, MultipartRejectionError> {
    let mut fields = Vec::new();

    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or_default().to_owned();
        let file_name = field.file_name().unwrap_or_default().to_owned();
        let content_type = field
            .content_type()
            .map(ToString::to_string)
            .unwrap_or_default();
        let data = field.bytes().await?;

        fields.push(format!(
            "name={name}; filename={file_name}; content_type={content_type}; bytes={}; body={}",
            data.len(),
            String::from_utf8_lossy(&data)
        ));
    }

    Ok(fields.join("\n"))
}

#[volo::main]
async fn main() -> Result<(), BoxError> {
    let app = Router::new().route("/upload", post(upload));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    let server = tokio::spawn(async move {
        let incoming = volo::net::DefaultIncoming::from(listener);
        let _ = Server::new(app).run(incoming).await;
    });

    let upload_file = write_upload_file().await?;
    let response = upload_once(addr, upload_file.path()).await?;
    println!("{response}");

    assert_eq!(
        response,
        "name=description; filename=; content_type=; bytes=25; body=streamed multipart \
         upload\nname=file; filename=volo-multipart-upload.txt; content_type=text/plain; \
         bytes=31; body=file content streamed from disk"
    );

    server.abort();
    Ok(())
}

async fn write_upload_file() -> Result<NamedTempFile, BoxError> {
    let upload_file = NamedTempFile::with_prefix("volo-multipart-upload")?;
    let path = upload_file.path().to_owned();
    let mut file = tokio::fs::File::create(&path).await?;
    file.write_all(b"file content streamed from disk").await?;
    file.flush().await?;
    Ok(upload_file)
}

async fn upload_once(addr: SocketAddr, path: &std::path::Path) -> Result<String, BoxError> {
    let client = volo_http::client::ClientBuilder::new().build()?;
    let url = format!("http://{addr}/upload");
    let mut last_err = None;
    for _ in 0..20 {
        let form = Form::new()
            .text("description", "streamed multipart upload")
            .part(
                "file",
                Part::file(path)
                    .await?
                    .file_name("volo-multipart-upload.txt")
                    .mime_str("text/plain")?,
            );

        match client.post(&url).multipart(form).send().await {
            Ok(resp) => {
                if resp.status() != StatusCode::OK {
                    return Err(format!("upload failed with status {}", resp.status()).into());
                }
                return Ok(resp.into_string().await?);
            }
            Err(err) => {
                last_err = Some(err);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    Err(last_err
        .map(|err| format!("server did not accept uploads: {err}"))
        .unwrap_or_else(|| "server did not accept uploads".to_owned())
        .into())
}
