use std::collections::HashMap;
use std::sync::Arc;

use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::auth::{AuthError, AuthorizationManager, CredentialStore};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use tokio::sync::Mutex;

use super::Auth;

type Error = StreamableHttpError<reqwest::Error>;
type Headers = HashMap<HeaderName, HeaderValue>;
type Events = BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;

#[derive(Clone)]
pub(crate) struct OAuthClient {
    client: reqwest::Client,
    manager: Arc<Mutex<AuthorizationManager>>,
    auth: Arc<Auth>,
    store: super::store::FileStore,
}

impl OAuthClient {
    pub(super) fn new(
        client: reqwest::Client,
        manager: AuthorizationManager,
        auth: Arc<Auth>,
        store: super::store::FileStore,
    ) -> Self {
        Self {
            client,
            manager: Arc::new(Mutex::new(manager)),
            auth,
            store,
        }
    }

    async fn token(&self) -> Result<Option<String>, Error> {
        let task = {
            let stopped = self.auth.token_gate.lock().map_err(|_poison| {
                AuthError::InternalError(String::from("MCP auth task lock poisoned"))
            })?;
            if *stopped {
                return Err(AuthError::AuthorizationRequired.into());
            }
            let manager = self.manager.clone();
            let task = self
                .auth
                .token_tasks
                .spawn(async move { manager.lock().await.get_access_token().await });
            drop(stopped);
            task
        };
        match task.await? {
            Ok(token) => {
                self.store.load().await?;
                Ok(Some(token))
            }
            Err(error) => {
                if matches!(
                    error,
                    AuthError::AuthorizationRequired | AuthError::TokenExpired
                ) && self.store.load().await.is_ok()
                {
                    self.auth
                        .required(
                            &self.store,
                            String::from("MCP credentials expired or were revoked; sign in again"),
                        )
                        .await;
                }
                Err(error.into())
            }
        }
    }

    async fn checked<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        if matches!(
            &result,
            Err(Error::AuthRequired(_) | Error::InsufficientScope(_))
        ) && self.store.load().await.is_ok()
        {
            self.auth
                .required(
                    &self.store,
                    String::from("MCP server requires login or additional scopes; sign in again"),
                )
                .await;
        }
        result
    }
}

impl StreamableHttpClient for OAuthClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth: Option<String>,
        headers: Headers,
    ) -> Result<StreamableHttpPostResponse, Error> {
        let token = self.token().await?;
        self.checked(
            self.client
                .post_message(uri, message, session_id, token, headers)
                .await,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth: Option<String>,
        headers: Headers,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, Error> {
        let token = self.token().await?;
        self.checked(
            self.client
                .post_message_with_max_sse_event_size(
                    uri,
                    message,
                    session_id,
                    token,
                    headers,
                    max_sse_event_size,
                )
                .await,
        )
        .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth: Option<String>,
        headers: Headers,
    ) -> Result<(), Error> {
        let token = self.token().await?;
        self.checked(
            self.client
                .delete_session(uri, session_id, token, headers)
                .await,
        )
        .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth: Option<String>,
        headers: Headers,
    ) -> Result<Events, Error> {
        let token = self.token().await?;
        self.checked(
            self.client
                .get_stream(uri, session_id, last_event_id, token, headers)
                .await,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth: Option<String>,
        headers: Headers,
        max_sse_event_size: usize,
    ) -> Result<Events, Error> {
        let token = self.token().await?;
        self.checked(
            self.client
                .get_stream_with_max_sse_event_size(
                    uri,
                    session_id,
                    last_event_id,
                    token,
                    headers,
                    max_sse_event_size,
                )
                .await,
        )
        .await
    }
}
