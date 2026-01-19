/// Macro to help implement LoadBalance trait for load balancers
#[macro_export]
macro_rules! impl_load_balance {
    ($ty:ty, $iter:ty) => {
        impl<D> LoadBalance<D> for $ty
        where
            D: Discover,
        {
            fn get_picker<'future>(
                &'future self,
                endpoint: &'future Endpoint,
                discover: &'future D,
            ) -> BoxFuture<
                'future,
                Result<Box<dyn Iterator<Item = Address> + Send>, LoadBalanceError>,
            > {
                Box::pin(async move {
                    let instances = discover
                        .discover(endpoint)
                        .await
                        .map_err(|_| LoadBalanceError::NoAvailableInstance)?;
                    if instances.is_empty() {
                        return Err(LoadBalanceError::NoAvailableInstance);
                    }
                    Ok(Box::new(<$iter>::new(instances)))
                })
            }
        }
    };
}
