use std::sync::{Arc, OnceLock};

use rmcp::transport::auth::{
    ClientRegistrationResponse, OAuthHttpClient, OAuthHttpClientFuture, OAuthHttpRequest,
};

use super::store::FileStore;

pub(super) struct RegistrationClient<C> {
    pub(super) inner: C,
    pub(super) endpoint: Arc<OnceLock<url::Url>>,
    pub(super) store: FileStore,
}

impl<C: OAuthHttpClient> OAuthHttpClient for RegistrationClient<C> {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            let registration = request.request.method() == http::Method::POST
                && request
                    .request
                    .headers()
                    .get(http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("application/json"))
                && self.endpoint.get().is_some_and(|endpoint| {
                    url::Url::parse(&request.request.uri().to_string())
                        .is_ok_and(|url| &url == endpoint)
                });
            let response = self.inner.execute(request).await?;
            if registration && response.status().is_success() {
                let registration: ClientRegistrationResponse =
                    serde_json::from_slice(response.body())?;
                self.store
                    .save_registration_secret(
                        registration
                            .client_secret
                            .filter(|secret| !secret.is_empty()),
                    )
                    .await?;
            }
            Ok(response)
        })
    }
}
