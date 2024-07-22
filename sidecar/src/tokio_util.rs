#[macro_export]
macro_rules! spawn_map_err {
    ($fut:expr, $err:expr) => {
        tokio::spawn(async move {
            if let Err(e) = tokio::spawn($fut).await {
                ($err)(e);
            }
        })
    };
}
