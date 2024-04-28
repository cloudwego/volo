#[async_trait::async_trait]
pub trait ShmExt: Send + Sync {
    fn release_read_and_reuse(&self) {}

    async fn close(&mut self) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn reuse(&self) {}
}
