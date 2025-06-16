use crate::context::Endpoint;
use crate::discovery::{Change, Discover, Instance};
use crate::loadbalance::error::LoadBalanceError;
use crate::net;
use crate::net::Address;
use anyhow::anyhow;
use async_broadcast::Receiver;
use faststr::FastStr;
use pd_rs_common::svc::nacos::NacosNamingAndConfigData;
use std::sync::Arc;
use tracing::warn;

#[derive(Clone)]
pub struct NacosDiscover {
    pub nacos_naming_data: Arc<NacosNamingAndConfigData>,
    pub svc_change_sender: async_broadcast::Sender<Change<FastStr>>,
    pub svc_change_receiver: async_broadcast::Receiver<Change<FastStr>>,
}

impl NacosDiscover {
    
    /// # create a nacos discover
    /// # Examples
    /// ```rust
    ///  // first new a NacosNamingAndConfigData
    ///  use std::sync::Arc;
    ///  use pd_rs_common::svc::nacos::NacosNamingAndConfigData;
    ///  use volo::discovery::nacos::NacosDiscover;
    ///  let nacos_data = Arc::new(
    ///      NacosNamingAndConfigData::new(
    ///          "127.0.0.1:8848".to_string(),  // nacos server addr.
    ///          "".to_string(),                // nacos namespace.
    ///          "myapp_name".to_string(),      // your app name.
    ///          None,                          // nacos server username if you need.
    ///          None,                          // nacos server password if you need.
    ///      )
    ///      .unwrap(),
    ///  );
    ///  // then register your self to nacos
    ///  nacos_data.register_service(
    ///     "myapp_name".to_string(),    // your service name, same as your app name generally.
    ///     8080,    // your service port.
    ///     None,    // service ip, it will get pod ip automatically if None.
    ///     None,    // group name, DEFAULT_GROUP if None.
    ///     Default::default()    // service metadata
    ///  ).await.unwrap();
    ///  // your other code ...
    ///
    ///  // finally new a nacos discover
    ///  let nacos_discover = NacosDiscover::new(nacos_data.clone());
    ///  // use nacos_discover with your code.
    /// ```
    /// ## See more: [volo-boot](https://github.com/intfish123/volo-boot/blob/master/api/src/bin/server.rs)
    pub fn new(inner: Arc<NacosNamingAndConfigData>) -> Self {
        let (mut svc_ch_s, svc_ch_r) = async_broadcast::broadcast(100);
        svc_ch_s.set_overflow(true);

        let ret = Self {
            nacos_naming_data: inner,
            svc_change_sender: svc_ch_s,
            svc_change_receiver: svc_ch_r,
        };

        let mut r = ret
            .nacos_naming_data
            .event_listener
            .sub_svc_change_receiver
            .clone();
        let s = ret.svc_change_sender.clone();
        tokio::spawn(async move {
            loop {
                match r.recv().await {
                    Ok(recv) => {
                        tracing::debug!("received svc change event: {:?}", recv);
                        let key: FastStr = recv.service_name.clone().into();
                        if let Some(is) = recv.instances.clone() {
                            let mut _ins_ret = vec![];
                            for x in is {
                                _ins_ret.push(Arc::new(Instance {
                                    address: net::Address::from(Address::Ip(
                                        format!("{}:{}", x.ip, x.port).parse().unwrap(),
                                    )),
                                    weight: x.weight as u32,
                                    tags: Default::default(),
                                }));
                            }

                            let ch = Change {
                                key: key,
                                all: _ins_ret,
                                added: Default::default(),
                                updated: Default::default(),
                                removed: Default::default(),
                            };
                            let _ = s.try_broadcast(ch);
                        }
                    }
                    Err(err) => warn!("nacos discovering subscription error: {:?}", err),
                }
            }
        });

        ret
    }
}

impl Discover for NacosDiscover {
    type Key = FastStr;
    type Error = LoadBalanceError;

    async fn discover<'s>(
        &'s self,
        endpoint: &'s Endpoint,
    ) -> Result<Vec<Arc<Instance>>, Self::Error> {
        let inst_list = self
            .nacos_naming_data
            .event_listener
            .sub_svc_map
            .get(endpoint.service_name.as_str());
        if let Some(inst_list) = inst_list {
            let mut _ins_ret = vec![];
            for x in inst_list.iter() {
                _ins_ret.push(Arc::new(Instance {
                    address: net::Address::from(Address::Ip(
                        format!("{}:{}", x.ip, x.port).parse().unwrap(),
                    )),
                    weight: x.weight as u32,
                    tags: Default::default(),
                }));
            }
            Ok(_ins_ret)
        } else {
            let ee = anyhow!("no instances for {}", endpoint.service_name.to_string()).into();
            Err(LoadBalanceError::Discover(ee))
        }
    }

    fn key(&self, endpoint: &Endpoint) -> Self::Key {
        endpoint.service_name.clone()
    }

    fn watch(&self, _keys: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        Some(self.svc_change_receiver.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::context::Endpoint;
    use crate::discovery::nacos::NacosDiscover;
    use crate::discovery::{Discover, Instance};
    use crate::net::Address;
    use pd_rs_common::svc::nacos::NacosNamingAndConfigData;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_nacos_discover() {
        // test with local environment
        // let _g = pd_rs_common::logger::init_tracing();
        // 
        // let nacos_data_ret = NacosNamingAndConfigData::new(
        //     "10.64.132.20:8848".to_string(),
        //     "public".to_string(),
        //     "volo-nacos-test".to_string(),
        //     None,
        //     None,
        // );
        // let nacos_data = match nacos_data_ret {
        //     Ok(data) => data,
        //     Err(e) => panic!("{:?}", e),
        // };
        // let nacos_data = Arc::new(nacos_data);
        // 
        // let _inst1 = nacos_data
        //     .register_service(
        //         "svc1".to_string(),
        //         8080,
        //         Some("172.1.0.1".to_string()),
        //         None,
        //         Default::default(),
        //     )
        //     .await
        //     .unwrap();
        // 
        // assert_eq!(_inst1.len(), 1);
        // 
        // nacos_data
        //     .subscribe_service("svc1".to_string())
        //     .await
        //     .unwrap();
        // 
        // 
        // // waiting for service change event. 
        // let s = tokio::spawn(async move {
        //     tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        // });
        // 
        // tokio::join!(s);
        // 
        // let nacos_discover = NacosDiscover::new(nacos_data);
        // let endpoint = Endpoint::new("svc1".into());
        // let resp = nacos_discover.discover(&endpoint).await.unwrap();
        // 
        // let expected = vec![Arc::new(Instance {
        //     address: Address::Ip("172.1.0.1:8080".parse().unwrap()),
        //     weight: 1,
        //     tags: Default::default(),
        // })];
        // assert_eq!(resp, expected);
    }
}
