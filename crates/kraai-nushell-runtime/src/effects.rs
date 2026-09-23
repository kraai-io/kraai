use kraai_types::StateEffectRequest;
use std::future::Future;
use std::pin::Pin;

pub trait StateEffectHandler: Send + Sync {
    fn apply<'a>(
        &'a self,
        request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

#[derive(Debug, Default)]
pub struct RejectStateEffects;

impl StateEffectHandler for RejectStateEffects {
    fn apply<'a>(
        &'a self,
        _request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async {
            Err(String::from(
                "state effects are not enabled for this execution",
            ))
        })
    }
}
