#![cfg(feature = "ttstream")]
use std::cell::Cell;

use pilota::thrift::{
    ApplicationException, ApplicationExceptionKind, Message, TAsyncInputProtocol, TInputProtocol,
    TLengthProtocol, TOutputProtocol, ThriftException,
};
use volo_thrift::{
    streaming::{Metadata, StreamConfig},
    transport::ttstream::connection::{Connection, StreamRouter},
};
struct NotSync(Cell<i32>);
fn payload(value: i32) -> ApplicationException {
    ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, value.to_string())
}
impl Message for NotSync {
    fn encode<P: TOutputProtocol>(&self, p: &mut P) -> Result<(), ThriftException> {
        payload(self.0.get()).encode(p)
    }
    fn decode<P: TInputProtocol>(p: &mut P) -> Result<Self, ThriftException> {
        let value = ApplicationException::decode(p)?;
        Ok(Self(Cell::new(value.message().parse().unwrap())))
    }
    async fn decode_async<P: TAsyncInputProtocol>(p: &mut P) -> Result<Self, ThriftException> {
        let value = ApplicationException::decode_async(p).await?;
        Ok(Self(Cell::new(value.message().parse().unwrap())))
    }
    fn size<P: TLengthProtocol>(&self, p: &mut P) -> usize {
        payload(self.0.get()).size(p)
    }
}
fn assert_send<T: Send>(_: &T) {}
#[tokio::test]
async fn borrowed_send_future_does_not_capture_non_sync_message() {
    let mut router = StreamRouter::new();
    router
        .add("Test", "Echo", |_, mut stream| async move {
            while let Some(bytes) = stream.recv_raw().await? {
                stream.send_raw(bytes).await?;
            }
            Ok(())
        })
        .unwrap();
    let (c, s) = tokio::io::duplex(1024);
    let (cr, cw) = tokio::io::split(c);
    let (sr, sw) = tokio::io::split(s);
    let client = Connection::start(cr, cw, StreamConfig::default(), None, None).unwrap();
    let server = Connection::start(sr, sw, StreamConfig::default(), None, Some(router)).unwrap();
    let (mut tx, mut rx) = client
        .open("Test", "Echo", Metadata::default())
        .await
        .unwrap()
        .into_bidi::<NotSync, NotSync>()
        .into_split();
    let future = {
        let message = NotSync(Cell::new(42));
        tx.send(&message)
    };
    assert_send(&future);
    future.await.unwrap();
    let reservation = tx.reserve().await.unwrap();
    reservation.send(&NotSync(Cell::new(43))).unwrap();
    // Releasing a permit neither half-closes nor consumes a message slot.
    drop(tx.reserve().await.unwrap());
    tx.close_send().await.unwrap();
    assert_eq!(rx.recv().await.unwrap().unwrap().0.get(), 42);
    assert_eq!(rx.recv().await.unwrap().unwrap().0.get(), 43);
    assert!(rx.recv().await.unwrap().is_none());
    client.close();
    server.close();
}
