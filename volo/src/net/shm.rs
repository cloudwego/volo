use crate::context::Endpoint;

crate::new_type! {
    #[derive(Debug, Hash, PartialEq, Eq, Clone)]
    pub struct Transport(pub faststr::FastStr);
}

pub trait TransportEndpoint {
    fn get_transport(&self) -> Option<Transport>;
    fn has_transport(&self) -> bool;
    fn set_transport(&mut self, transport: Transport);
}

impl TransportEndpoint for Endpoint {
    #[inline]
    fn get_transport(&self) -> Option<Transport> {
        self.get_faststr::<Transport>()
            .cloned()
            .map(Transport::from)
    }

    #[inline]
    fn has_transport(&self) -> bool {
        self.contains_faststr::<Transport>()
    }

    #[inline]
    fn set_transport(&mut self, transport: Transport) {
        self.insert_faststr::<Transport>(transport.0);
    }
}

#[async_trait::async_trait]
pub trait ShmExt: Send + Sync {
    fn release_read_and_reuse(&self) {}

    async fn close(&mut self) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn reuse(&self) {}
}
